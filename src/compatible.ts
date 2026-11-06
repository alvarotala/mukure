/** Feed FFmpeg's fragmented MP4 to the WebView without waiting for a finished file. */
const CODECS = [
  'video/mp4; codecs="avc1.42E01E, mp4a.40.2"',
  'video/mp4; codecs="avc1.42C01F, mp4a.40.2"',
];
const BUFFER_AHEAD = 20;
const KEEP_BEHIND = 10;
const MIN_KEEP_BEHIND = 3;
const MAX_APPEND_BYTES = 256 * 1024;
const MIN_APPEND_BYTES = 16 * 1024;
const QUOTA_STALL_TIMEOUT = 15000;

function quotaExceeded(reason: unknown): boolean {
  return typeof reason === 'object' && reason !== null && 'name' in reason && reason.name === 'QuotaExceededError';
}

function bufferFull(): Error {
  return new Error('El buffer de video del reproductor está lleno y no se pudo liberar espacio. Probá el modo Directo o volvé a abrir el video.');
}

function aborted(): DOMException {
  return new DOMException('Reproducción cancelada', 'AbortError');
}

function waitForOpen(source: MediaSource, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal.aborted) { reject(aborted()); return; }
    if (source.readyState === 'open') { resolve(); return; }
    const cleanup = () => {
      clearTimeout(timer);
      source.removeEventListener('sourceopen', opened);
      source.removeEventListener('sourceclose', closed);
      signal.removeEventListener('abort', cancelled);
    };
    const opened = () => { cleanup(); resolve(); };
    const closed = () => { cleanup(); reject(new Error('El WebView cerró MediaSource antes de abrir el video.')); };
    const cancelled = () => { cleanup(); reject(aborted()); };
    const timer = setTimeout(() => {
      cleanup();
      reject(new Error('El WebView no pudo abrir MediaSource para reproducir la conversión.'));
    }, 15000);
    source.addEventListener('sourceopen', opened);
    source.addEventListener('sourceclose', closed);
    signal.addEventListener('abort', cancelled, { once: true });
  });
}

/** appendBuffer and remove must never run concurrently on the same SourceBuffer. */
function updateBuffer(buffer: SourceBuffer, source: MediaSource, signal: AbortSignal, update: () => void): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal.aborted) { reject(aborted()); return; }
    const cleanup = () => {
      buffer.removeEventListener('updateend', finished);
      buffer.removeEventListener('error', failed);
      buffer.removeEventListener('abort', cancelled);
      source.removeEventListener('sourceclose', closed);
      signal.removeEventListener('abort', cancelled);
    };
    const finished = () => { cleanup(); resolve(); };
    const failed = () => { cleanup(); reject(new Error('El WebView no pudo decodificar el fragmento de video generado por FFmpeg.')); };
    const cancelled = () => { cleanup(); reject(aborted()); };
    const closed = () => { cleanup(); reject(new Error('Se cerró el reproductor de la conversión.')); };
    buffer.addEventListener('updateend', finished);
    buffer.addEventListener('error', failed);
    buffer.addEventListener('abort', cancelled);
    source.addEventListener('sourceclose', closed);
    signal.addEventListener('abort', cancelled, { once: true });
    try { update(); }
    catch (reason) { cleanup(); reject(reason); }
  });
}

function waitForPlayback(video: HTMLVideoElement, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal.aborted) { reject(aborted()); return; }
    const cleanup = () => {
      clearTimeout(timer);
      video.removeEventListener('timeupdate', continued);
      video.removeEventListener('seeking', continued);
      signal.removeEventListener('abort', cancelled);
    };
    const continued = () => { cleanup(); resolve(); };
    const cancelled = () => { cleanup(); reject(aborted()); };
    const timer = setTimeout(continued, 300);
    video.addEventListener('timeupdate', continued, { once: true });
    video.addEventListener('seeking', continued, { once: true });
    signal.addEventListener('abort', cancelled, { once: true });
  });
}

export function attachCompatibleVideo(video: HTMLVideoElement, url: string, onError: (message: string) => void): () => void {
  if (typeof MediaSource === 'undefined') {
    onError('Este WebView no dispone de MediaSource, necesario para el modo FFmpeg. Probá el modo directo o actualizá el sistema.');
    return () => {};
  }
  const mime = CODECS.find((type) => MediaSource.isTypeSupported(type));
  if (!mime) {
    onError('Este WebView no admite H.264 y AAC mediante MediaSource. El modo FFmpeg no está disponible en esta instalación.');
    return () => {};
  }

  const source = new MediaSource();
  const objectUrl = URL.createObjectURL(source);
  const controller = new AbortController();
  const { signal } = controller;
  let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
  let buffer: SourceBuffer | undefined;
  let failed = false;

  const finish = (error?: EndOfStreamError) => {
    if (source.readyState !== 'open') return;
    try {
      if (buffer?.updating) buffer.abort();
      source.endOfStream(error);
    } catch { /* Detaching the element may already have closed MediaSource. */ }
  };
  const fail = (message: string) => {
    if (signal.aborted || failed) return;
    failed = true;
    onError(message);
    controller.abort();
    void reader?.cancel().catch(() => {});
    finish('decode');
  };
  const mediaFailed = () => fail(`El WebView no pudo reproducir la conversión de FFmpeg${video.error?.message ? `: ${video.error.message}` : '.'}`);

  // A removed range cannot be reloaded: FFmpeg supplies this stream sequentially.
  const keepSeekInBuffer = () => {
    if (!buffer?.buffered.length) return;
    const first = buffer.buffered.start(0);
    const last = buffer.buffered.end(buffer.buffered.length - 1);
    if (video.currentTime < first) video.currentTime = first + 0.01;
    else if (video.currentTime > last) video.currentTime = Math.max(first, last - 0.1);
  };
  video.addEventListener('error', mediaFailed);
  video.addEventListener('seeking', keepSeekInBuffer);

  const opened = waitForOpen(source, signal);
  video.src = objectUrl;
  video.load();

  async function stream() {
    await opened;
    if (signal.aborted) throw aborted();
    buffer = source.addSourceBuffer(mime!);
    const response = await fetch(url, { signal, cache: 'no-store', credentials: 'omit' });
    if (!response.ok) {
      const details = (await response.text()).slice(0, 300);
      throw new Error(`FFmpeg no pudo iniciar la conversión (HTTP ${response.status})${details ? `: ${details}` : '.'}`);
    }
    if (!response.body) throw new Error('El WebView no permite leer la salida de FFmpeg como un stream.');
    reader = response.body.getReader();
    let receivedBytes = 0;
    let aheadLimit = BUFFER_AHEAD;
    let keepBehind = KEEP_BEHIND;
    let appendSize = MAX_APPEND_BYTES;
    let recoveringQuota = false;
    let stalledFor = 0;

    const bufferedAhead = () => {
      const ranges = buffer!.buffered;
      // Only playable data around the current position can unblock an append.
      for (let i = 0; i < ranges.length; i++) {
        if (ranges.start(i) <= video.currentTime + 0.1 && ranges.end(i) > video.currentTime) {
          return ranges.end(i) - video.currentTime;
        }
      }
      return 0;
    };
    const trimPlayed = async () => {
      const ranges = buffer!.buffered;
      const removeTo = video.currentTime - keepBehind;
      if (!ranges.length || removeTo <= ranges.start(0) + 0.5) return false;
      const previousStart = ranges.start(0);
      await updateBuffer(buffer!, source, signal, () => buffer!.remove(0, removeTo));
      return !buffer!.buffered.length || buffer!.buffered.start(0) > previousStart;
    };
    const waitForSpace = async () => {
      const position = video.currentTime;
      const paused = video.paused;
      const started = performance.now();
      await waitForPlayback(video, signal);
      if (!recoveringQuota || paused || video.paused || video.currentTime > position + 0.01) stalledFor = 0;
      else stalledFor += performance.now() - started;
      if (stalledFor >= QUOTA_STALL_TIMEOUT) throw bufferFull();
    };
    const makeRoom = async () => {
      // Trim even while the read-ahead limit is reached or playback is paused.
      await trimPlayed();
      while (bufferedAhead() > aheadLimit) {
        await waitForSpace();
        await trimPlayed();
      }
    };
    const appendChunk = async (value: Uint8Array) => {
      let offset = 0;
      while (offset < value.byteLength) {
        if (signal.aborted) throw aborted();
        await makeRoom();
        const length = Math.min(appendSize, value.byteLength - offset);
        const chunk = new Uint8Array(length);
        chunk.set(value.subarray(offset, offset + length));
        try {
          await updateBuffer(buffer!, source, signal, () => buffer!.appendBuffer(chunk));
          // A synchronous quota exception accepts none of the bytes. Advance
          // only after updateend, so retries neither drop nor duplicate data.
          offset += length;
          recoveringQuota = false;
          stalledFor = 0;
        } catch (reason) {
          if (!quotaExceeded(reason)) throw reason;
          recoveringQuota = true;
          aheadLimit = Math.max(2, aheadLimit / 2);
          keepBehind = MIN_KEEP_BEHIND;
          if (await trimPlayed()) continue;
          if (length > MIN_APPEND_BYTES) {
            appendSize = Math.max(MIN_APPEND_BYTES, Math.floor(length / 2));
            continue;
          }
          if (bufferedAhead() <= 0.25) throw bufferFull();
          // Keep the pending bytes and stop reading the response until playback
          // frees memory. Pausing is allowed; detaching cancels this wait.
          await waitForSpace();
        }
      }
    };
    try {
      while (!signal.aborted) {
        await makeRoom();
        const { done, value } = await reader.read();
        if (done) {
          if (!receivedBytes) throw new Error('FFmpeg cerró la conversión sin generar video.');
          finish();
          return;
        }
        if (!value.byteLength) continue;
        receivedBytes += value.byteLength;
        await appendChunk(value);
      }
    } finally {
      reader.releaseLock();
    }
  }

  void stream().catch((reason: unknown) => {
    if (!signal.aborted) fail(reason instanceof Error ? reason.message : 'Falló la reproducción de la conversión de FFmpeg.');
  });

  return () => {
    video.removeEventListener('error', mediaFailed);
    video.removeEventListener('seeking', keepSeekInBuffer);
    controller.abort();
    void reader?.cancel().catch(() => {});
    finish();
    if (video.getAttribute('src') === objectUrl) {
      video.pause();
      video.removeAttribute('src');
      video.load();
    }
    URL.revokeObjectURL(objectUrl);
  };
}
