/** Feed FFmpeg's fragmented MP4 to the WebView without waiting for a finished file. */
const CODECS = [
  'video/mp4; codecs="avc1.42E01E, mp4a.40.2"',
  'video/mp4; codecs="avc1.42C01F, mp4a.40.2"',
];
const BUFFER_AHEAD = 30;
const KEEP_BEHIND = 30;

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
    try {
      while (!signal.aborted) {
        const ranges = buffer.buffered;
        if (ranges.length && ranges.end(ranges.length - 1) - video.currentTime > BUFFER_AHEAD) {
          await waitForPlayback(video, signal);
          continue;
        }
        const removeTo = video.currentTime - KEEP_BEHIND;
        if (ranges.length && removeTo > 0 && removeTo - ranges.start(0) > 5) {
          await updateBuffer(buffer, source, signal, () => buffer!.remove(0, removeTo));
        }
        const { done, value } = await reader.read();
        if (done) {
          if (!receivedBytes) throw new Error('FFmpeg cerró la conversión sin generar video.');
          finish();
          return;
        }
        if (!value.byteLength) continue;
        receivedBytes += value.byteLength;
        // Copy to an ArrayBuffer backed view, including on engines with shared-buffer typings.
        const chunk = new Uint8Array(value.byteLength);
        chunk.set(value);
        await updateBuffer(buffer, source, signal, () => buffer!.appendBuffer(chunk));
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
