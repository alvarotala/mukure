import { useEffect, useRef, useState, type RefObject } from 'react';
import { Channel, invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { Captions, Download, FolderOpen, Globe, LoaderCircle, Search, Settings2, X } from 'lucide-react';
import { createSubtitleRenderer, subtitleWebVtt, type SubtitleRenderer } from './subtitles';
import { errorMessage, fileName } from './format';
import type { EmbeddedSubtitleTrack, OnlineSubtitle, Playback, SubtitleEvent, SubtitleSearchResult, SubtitleTrack, VideoFile } from './types';

interface Props {
  videoRef: RefObject<HTMLVideoElement | null>;
  playback: Playback | null;
  torrentId: number | undefined;
  files: VideoFile[];
  disabled: boolean;
  localPath: string | null;
  providers: string[];
  onProviders: () => void;
  onCastSubtitle: (subtitle: string | null) => void;
}

export default function SubtitleControls({ videoRef, playback, torrentId, files, disabled, localPath, providers, onProviders, onCastSubtitle }: Props) {
  const [subtitles, setSubtitles] = useState<SubtitleTrack | null>(null);
  const [subtitleIdentity, setSubtitleIdentity] = useState('');
  const [enabled, setEnabled] = useState(true);
  const [offset, setOffset] = useState(0);
  const [selection, setSelection] = useState('');
  const [loading, setLoading] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [embeddedTracks, setEmbeddedTracks] = useState<EmbeddedSubtitleTrack[] | null>(null);
  const [embeddedChoice, setEmbeddedChoice] = useState<EmbeddedSubtitleTrack | null>(null);
  const [probing, setProbing] = useState(false);
  const [probeError, setProbeError] = useState<string | null>(null);
  const [extracting, setExtracting] = useState(false);
  const [seekRestart, setSeekRestart] = useState(0);
  const subtitlesRef = useRef<SubtitleTrack | null>(null);
  const identityRef = useRef('');
  const rendererRef = useRef<{ id: string; renderer: SubtitleRenderer } | null>(null);
  const request = useRef(0);
  const probeRequest = useRef(0);
  const extraction = useRef(0);
  const extractionCommands = useRef<Promise<void>>(Promise.resolve());
  const previousOffset = useRef(offset);
  const loadingRef = useRef(false);
  const unavailable = disabled || !playback;
  const [online, setOnline] = useState(() => localStorage.getItem('mukure.subtitle.online') !== 'false');
  const [language, setLanguage] = useState(() => localStorage.getItem('mukure.subtitle.language') || 'es');
  const [query, setQuery] = useState('');
  const [submittedQuery, setSubmittedQuery] = useState('');
  const [retry, setRetry] = useState(0);
  const [results, setResults] = useState<Record<string, OnlineSubtitle[]>>({});
  const [sources, setSources] = useState<Record<string, string>>({});
  const [nearby, setNearby] = useState<{ name: string; path: string }[]>([]);
  const providerList = ['opensubtitles', 'subtitledb', ...providers].join(',');

  useEffect(() => { localStorage.setItem('mukure.subtitle.online', String(online)); }, [online]);
  useEffect(() => { localStorage.setItem('mukure.subtitle.language', language); }, [language]);
  useEffect(() => { if (playback && !query) setQuery(fileName(playback.name)); }, [playback?.name]);

  useEffect(() => {
    if (!playback || disabled) return;
    // Delay one tick so StrictMode's setup/cleanup does not launch two probes.
    const timer = setTimeout(() => void findEmbeddedTracks(), 200);
    return () => clearTimeout(timer);
  }, [playback?.url, disabled]);

  useEffect(() => {
    if (!localPath || disabled) return;
    let cancelled = false;
    invoke<{ name: string; path: string }[]>('find_local_subtitles', { path: localPath })
      .then((items) => { if (!cancelled) setNearby(items); })
      .catch((reason: unknown) => { if (!cancelled) setError(errorMessage(reason)); });
    return () => { cancelled = true; };
  }, [localPath, disabled]);

  useEffect(() => {
    setResults({}); setSources({});
    if (!playback || disabled || !online) return;
    let cancelled = false;
    const ids = providerList.split(',');
    const timer = setTimeout(() => {
      setSources(Object.fromEntries(ids.map((id) => [id, 'Buscando…'])));
      for (const id of ids) {
        invoke<SubtitleSearchResult>('search_online_subtitles', {
          playbackUrl: playback.url, provider: id, query: submittedQuery || fileName(playback.name), language,
        }).then((data) => {
          if (cancelled) return;
          setResults((old) => ({ ...old, [id]: data.subtitles }));
          setSources((old) => ({ ...old, [id]: data.subtitles.length ? `${data.subtitles.length} encontrados` : 'Sin resultados en este idioma' }));
        }).catch((reason: unknown) => {
          if (!cancelled) setSources((old) => ({ ...old, [id]: errorMessage(reason) }));
        });
      }
    }, 350);
    return () => { cancelled = true; clearTimeout(timer); };
  }, [playback?.url, disabled, online, language, submittedQuery, retry, providerList]);

  function pickOnline(track: OnlineSubtitle) {
    if (!playback) return;
    void load(`Descargando desde ${track.provider}…`, async () => ({
      track: await invoke<SubtitleTrack>('download_online_subtitle', { playbackUrl: playback.url, url: track.url, name: track.name }),
      selection: track.id,
    }));
  }


  useEffect(() => {
    onCastSubtitle(subtitles?.cues.length && enabled && !embeddedChoice ? subtitleWebVtt(subtitles, offset) : null);
  }, [subtitles, enabled, offset, embeddedChoice, onCastSubtitle]);
  useEffect(() => () => onCastSubtitle(null), [onCastSubtitle]);

  function updateSubtitles(track: SubtitleTrack, id: string) {
    subtitlesRef.current = track;
    identityRef.current = id;
    setSubtitleIdentity(id);
    setSubtitles(track);
  }

  useEffect(() => {
    ++request.current;
    ++probeRequest.current;
    setLoading(null);
    setProbing(false);
    loadingRef.current = false;
    return () => { ++request.current; ++probeRequest.current; };
  }, [playback?.url]);

  useEffect(() => {
    if (!playback || !embeddedChoice) return;
    const playbackUrl = playback.url;
    const position = videoRef.current?.currentTime ?? 0;
    // Extraction seeks in the original subtitle timeline, before the display
    // adjustment is applied. Positive delays can need cues well before now.
    const startTime = Number.isFinite(position) ? Math.max(0, position - offset) : 0;
    const requestId = crypto.randomUUID();
    const ticket = ++extraction.current;
    updateSubtitles({ name: embeddedChoice.name, cues: [] }, requestId);
    setExtracting(true);
    setError(null);
    const onEvent = new Channel<SubtitleEvent>();
    onEvent.onmessage = (event) => {
      if (extraction.current !== ticket) return;
      if (event.type === 'cues') {
        const previous = subtitlesRef.current;
        if (!previous || identityRef.current !== requestId) return;
        const track = { ...previous, cues: [...previous.cues, ...event.cues] };
        subtitlesRef.current = track;
        setSubtitles(track);
        try {
          if (rendererRef.current?.id === requestId) rendererRef.current.renderer.append(event.cues);
        } catch (reason) { setError(errorMessage(reason)); }
      } else {
        setExtracting(false);
        if (event.type === 'error') setError(event.message);
      }
    };
    extractionCommands.current = extractionCommands.current.then(async () => {
      if (extraction.current !== ticket) return;
      await invoke('start_embedded_subtitle', { playbackUrl, trackId: embeddedChoice.id, requestId, startTime, onEvent });
    })
      .catch((reason: unknown) => {
        if (extraction.current === ticket) { setExtracting(false); setError(errorMessage(reason)); }
      });
    return () => {
      ++extraction.current;
      setExtracting(false);
      // Matching both URL and request ID keeps an old cleanup from stopping
      // a newly selected embedded track on the same video.
      // Serialize start/stop as well: cleanup can run before an asynchronous
      // start command has acknowledged registration of its background job.
      extractionCommands.current = extractionCommands.current
        .then(() => invoke<void>('stop_embedded_subtitle', { playbackUrl, requestId })).catch(() => {});
    };
  }, [playback?.url, embeddedChoice, videoRef, seekRestart]);

  useEffect(() => {
    const video = videoRef.current;
    if (!video || !playback || !embeddedChoice) return;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const seek = () => {
      clearTimeout(timer);
      // Scrubbing and MSE's buffered-range clamp can emit several seeking
      // events. Restart only after the target time has settled.
      timer = setTimeout(() => setSeekRestart((value) => value + 1), 250);
    };
    video.addEventListener('seeking', seek);
    return () => { video.removeEventListener('seeking', seek); clearTimeout(timer); };
  }, [videoRef, playback?.url, embeddedChoice]);

  useEffect(() => {
    if (previousOffset.current === offset) return;
    previousOffset.current = offset;
    if (!embeddedChoice) return;
    const timer = setTimeout(() => setSeekRestart((value) => value + 1), 250);
    return () => clearTimeout(timer);
  }, [offset, embeddedChoice]);

  useEffect(() => {
    const video = videoRef.current;
    if (!video || !playback || !subtitleIdentity) return;
    let renderer: SubtitleRenderer | undefined;
    const install = () => {
      const track = subtitlesRef.current;
      if (!track || identityRef.current !== subtitleIdentity) return;
      try {
        renderer = createSubtitleRenderer(video, track.name, offset, enabled);
        renderer.append(track.cues);
        rendererRef.current = { id: subtitleIdentity, renderer };
      } catch (reason) { setError(errorMessage(reason)); }
    };
    // New playback calls video.load(), which resets its media state. Wait for
    // metadata before installing cues, including when switching to FFmpeg/MSE.
    if (video.readyState >= HTMLMediaElement.HAVE_METADATA) install();
    else video.addEventListener('loadedmetadata', install, { once: true });
    return () => {
      video.removeEventListener('loadedmetadata', install);
      if (rendererRef.current?.renderer === renderer) rendererRef.current = null;
      renderer?.detach();
    };
  }, [videoRef, playback?.url, subtitleIdentity, offset, enabled]);

  async function load(label: string, read: () => Promise<{ track: SubtitleTrack; selection: string } | null>) {
    if (unavailable || loadingRef.current) return;
    const ticket = ++request.current;
    loadingRef.current = true;
    setLoading(label);
    setError(null);
    try {
      const result = await read();
      if (ticket !== request.current || !result) return;
      // Read first: a cancelled picker or malformed file keeps the current
      // track, including any progressive embedded extraction, intact.
      ++extraction.current;
      setEmbeddedChoice(null);
      setExtracting(false);
      updateSubtitles(result.track, crypto.randomUUID());
      setSelection(result.selection);
      setEnabled(true);
      setOffset(0);
    } catch (reason) {
      if (ticket === request.current) setError(errorMessage(reason));
    } finally {
      if (ticket === request.current) { setLoading(null); loadingRef.current = false; }
    }
  }

  function pickFile() {
    void load('Abriendo subtítulos…', async () => {
      const path = await open({ multiple: false, directory: false, filters: [{ name: 'Subtítulos', extensions: ['srt', 'vtt'] }] });
      if (typeof path !== 'string') return null;
      return { track: await invoke<SubtitleTrack>('read_subtitle_file', { path }), selection: 'local' };
    });
  }

  function pickTorrentFile(value: string) {
    if (!value || torrentId === undefined) return;
    const fileId = Number(value);
    if (!Number.isInteger(fileId) || !files.some((file) => file.id === fileId)) return;
    void load('Descargando subtítulos del torrent…', async () => ({
      track: await invoke<SubtitleTrack>('read_torrent_subtitle', { id: torrentId, fileId }),
      selection: value,
    }));
  }

  async function findEmbeddedTracks() {
    if (unavailable || probing || !playback) return;
    const ticket = ++probeRequest.current;
    setProbing(true);
    setProbeError(null);
    try {
      const tracks = await invoke<EmbeddedSubtitleTrack[]>('get_embedded_subtitles', { playbackUrl: playback.url });
      if (probeRequest.current === ticket) setEmbeddedTracks(tracks);
    } catch (reason) {
      if (probeRequest.current === ticket) setProbeError(errorMessage(reason));
    } finally {
      if (probeRequest.current === ticket) setProbing(false);
    }
  }

  function pickEmbeddedTrack(value: string) {
    const choice = embeddedTracks?.find((track) => String(track.id) === value && track.supported);
    if (!choice || unavailable || loading) return;
    setEmbeddedChoice(choice);
    setSelection(`embedded:${choice.id}`);
    setEnabled(true);
    setOffset(0);
  }

  return (
    <section className="subtitle-panel" aria-label="Subtítulos">
      <div className="subtitle-heading"><Captions size={18} /><div><strong>Subtítulos</strong><p>Búsqueda automática en tus archivos y en Internet.</p></div></div>
      <div className="online-subtitle-options">
        <label className="subtitle-toggle"><input type="checkbox" checked={online} onChange={(event) => setOnline(event.target.checked)} />Buscar en Internet automáticamente</label>
        <select aria-label="Idioma de subtítulos" value={language} onChange={(event) => setLanguage(event.target.value)}>{[['es', 'Español'], ['en', 'English'], ['pt', 'Português'], ['fr', 'Français'], ['it', 'Italiano'], ['de', 'Deutsch']].map(([id, label]) => <option key={id} value={id}>{label}</option>)}</select>
        <button className="subtitle-open" onClick={onProviders}><Settings2 size={14} />Conectar fuentes</button>
      </div>
      {online && <form className="subtitle-search" onSubmit={(event) => { event.preventDefault(); setSubmittedQuery(query.trim()); setRetry((n) => n + 1); }}><Search size={15} /><input aria-label="Título para buscar subtítulos" value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Título y año, o IMDb · series: S01E01" /><button className="subtitle-open" disabled={unavailable || !query.trim()} type="submit">Buscar</button></form>}
      {online && <p className="subtitle-hint">Se consulta el nombre del video; el archivo y su ruta local permanecen en tu equipo. Podés corregir el título arriba.</p>}
      {Object.keys(sources).length > 0 && <div className="subtitle-sources" aria-live="polite">{Object.entries(sources).map(([id, status]) => <div key={id}>{status === 'Buscando…' ? <LoaderCircle size={12} className="spin" /> : <Globe size={12} />}<strong>{{ opensubtitles: 'OpenSubtitles', subtitledb: 'SubtitleDB', subdl: 'SubDL', wyzie: 'Wyzie' }[id]}</strong><span>{status}</span></div>)}</div>}
      {Object.values(results).some((items) => items.length > 0) && <div className="online-results">{Object.values(results).flat().map((track) => <button key={track.id} className={`subtitle-result ${selection === track.id ? 'selected' : ''}`} disabled={unavailable || Boolean(loading)} onClick={() => pickOnline(track)}><span><strong>{track.name || 'Subtítulos'}</strong><small>{track.provider} · {track.language.toUpperCase()}</small></span>{selection === track.id ? <Captions size={17} /> : <Download size={16} />}</button>)}</div>}
      <div className="subtitle-actions">
        <button className="subtitle-open" onClick={pickFile} disabled={unavailable || Boolean(loading)}><FolderOpen size={14} />Abrir subtítulos</button>
        {nearby.length > 0 && <select aria-label="Subtítulos junto al video" value={selection.startsWith('sidecar:') ? selection.slice(8) : ''} disabled={unavailable || Boolean(loading)} onChange={(event) => { const path = event.target.value; if (path) void load('Abriendo subtítulos…', async () => ({ track: await invoke<SubtitleTrack>('read_subtitle_file', { path }), selection: `sidecar:${path}` })); }}><option value="">En la carpeta del video…</option>{nearby.map((file) => <option key={file.path} value={file.path}>{file.name}</option>)}</select>}
        {files.length > 0 && <select aria-label="Subtítulos del torrent" value={/^\d+$/.test(selection) ? selection : ''} disabled={unavailable || Boolean(loading)} onChange={(event) => pickTorrentFile(event.target.value)}><option value="">Elegir del torrent…</option>{files.map((file) => <option key={file.id} value={file.id}>{fileName(file.name)}</option>)}</select>}
        <button className="subtitle-open" onClick={() => void findEmbeddedTracks()} disabled={unavailable || probing || Boolean(loading)}>{probing ? <LoaderCircle size={14} className="spin" /> : <Search size={14} />}{probing ? 'Buscando pistas…' : probeError ? 'Reintentar búsqueda' : 'Buscar pistas integradas'}</button>
        {embeddedTracks && embeddedTracks.length > 0 && <select aria-label="Pistas de subtítulos integradas" value={selection.startsWith('embedded:') ? selection.slice(9) : ''} disabled={unavailable || Boolean(loading)} onChange={(event) => pickEmbeddedTrack(event.target.value)}><option value="">Elegir pista integrada…</option>{embeddedTracks.map((track) => <option key={track.id} value={track.id} disabled={!track.supported}>{track.name}{track.language && track.language !== 'und' ? ` · ${track.language}` : ''} · {track.codec}{!track.supported ? ' · no compatible' : ''}</option>)}</select>}
      </div>
      {embeddedTracks?.length === 0 && <p className="subtitle-hint">Este video no tiene pistas de subtítulos integradas.</p>}
      {embeddedTracks?.some((track) => !track.supported) && <p className="subtitle-hint">Las pistas de subtítulos en formato de imagen todavía no son compatibles.</p>}
      {loading && <p className="subtitle-loading" role="status"><LoaderCircle size={13} className="spin" />{loading}</p>}
      {extracting && <p className="subtitle-loading" role="status"><LoaderCircle size={13} className="spin" />Extrayendo pista integrada mientras llega el video…</p>}
      {subtitles && <div className="subtitle-selected"><span className="subtitle-name" title={subtitles.name}>{fileName(subtitles.name)}<small>{subtitles.cues.length.toLocaleString('es')} entradas</small></span><label className="subtitle-toggle"><input type="checkbox" checked={enabled} disabled={unavailable} onChange={(event) => setEnabled(event.target.checked)} />Mostrar</label><label className="subtitle-offset">Ajuste<input aria-label="Ajuste de subtítulos en segundos" type="number" min="-60" max="60" step="0.5" value={offset} disabled={unavailable} onChange={(event) => { const value = event.target.valueAsNumber; if (Number.isFinite(value)) setOffset(Math.min(60, Math.max(-60, value))); }} /><span>s</span></label><button className="subtitle-reset" disabled={unavailable || offset === 0} onClick={() => setOffset(0)}>Restablecer</button></div>}
      {subtitles && <p className="subtitle-offset-hint">Ajuste positivo: aparecen más tarde. Negativo: aparecen antes.</p>}
      {!playback && <p className="subtitle-hint">Abrí un video para agregar subtítulos.</p>}
      {(error || probeError) && <div className="subtitle-error" role="alert"><span>{error || probeError}</span><button aria-label="Cerrar error de subtítulos" onClick={() => { setError(null); setProbeError(null); }}><X size={14} /></button></div>}
    </section>
  );
}
