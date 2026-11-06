import { useCallback, useEffect, useRef, useState, type FormEvent } from 'react';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import {
  ArrowDown, ArrowUp, Captions, Cast, Check, CircleHelp, FileVideo2, FolderOpen,
  KeyRound, Link2, ListVideo, LoaderCircle, MonitorPlay, Play,
  Radio, Settings2, Square, Users, X,
} from 'lucide-react';
import PlayerControls from './PlayerControls';
import CastPanel from './CastPanel';
import { useCasting } from './useCasting';
import { usePlayerChrome } from './usePlayerChrome';
import ProviderSettings, { type ProviderStatus } from './ProviderSettings';
import { errorMessage, fileName, formatBytes } from './format';
import { attachCompatibleVideo } from './compatible';
import SubtitleControls from './SubtitleControls';
import type { AppInfo, Playback, PlaybackMode, TorrentStats, TorrentView, VideoFile } from './types';

const desktop = isTauri();

function App() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [magnet, setMagnet] = useState('');
  const [torrent, setTorrent] = useState<TorrentView | null>(null);
  const [localPath, setLocalPath] = useState<string | null>(null);
  const [playback, setPlayback] = useState<Playback | null>(null);
  const [stats, setStats] = useState<TorrentStats | null>(null);
  const [mode, setMode] = useState<PlaybackMode>('direct');
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [statsError, setStatsError] = useState<string | null>(null);
  const [buffering, setBuffering] = useState(false);
  const [mediaReady, setMediaReady] = useState(false);
  const [panel, setPanel] = useState<'open' | 'subtitles' | 'providers' | 'files' | 'help' | 'cast' | null>(null);
  const [castSubtitle, setCastSubtitle] = useState<string | null>(null);
  const [providers, setProviders] = useState<ProviderStatus[]>([]);
  const [fullscreen, setFullscreen] = useState(false);
  const chromeHidden = usePlayerChrome(fullscreen, Boolean(panel || busy || error));
  const refreshProviders = useCallback(async () => {
    if (!desktop) return;
    const statuses = await invoke<ProviderStatus[]>('get_subtitle_providers');
    setProviders(statuses);
  }, []);
  const videoRef = useRef<HTMLVideoElement>(null);
  const casting = useCasting(videoRef, playback);
  const operation = useRef(0);
  const busyRef = useRef(false);
  const sourceTitle = playback?.name || torrent?.name || (localPath ? fileName(localPath) : null);
  const currentFile = torrent?.files.find((file) => file.id === torrent.selectedFile);
  const progress = stats?.totalBytes ? Math.min(100, (stats.progressBytes / stats.totalBytes) * 100) : 0;
  const hasSource = Boolean(torrent || localPath);
  const disabled = !desktop || Boolean(busy);

  useEffect(() => {
    if (!desktop) return;
    let cancelled = false;
    void refreshProviders().catch((reason: unknown) => setError(errorMessage(reason)));
    invoke<AppInfo>('get_app_info').then((data) => { if (!cancelled) setInfo(data); })
      .catch((reason: unknown) => { if (!cancelled) setError(errorMessage(reason)); });
    return () => { cancelled = true; };
  }, []);

  useEffect(() => {
    if (!torrent || !desktop) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;
    const id = torrent.id;
    async function poll() {
      try {
        const data = await invoke<TorrentStats>('get_torrent_stats', { id });
        if (!cancelled) { setStats(data); setStatsError(null); }
      } catch (reason) {
        if (!cancelled) setStatsError(errorMessage(reason));
      } finally {
        if (!cancelled) timer = setTimeout(poll, 1000);
      }
    }
    void poll();
    return () => { cancelled = true; clearTimeout(timer); };
  }, [torrent?.id]);

  useEffect(() => {
    setBuffering(Boolean(playback));
    setMediaReady(false);
    const video = videoRef.current;
    if (!video || !playback) return;
    let detach: (() => void) | undefined;
    if (playback.mode === 'compatible') {
      detach = attachCompatibleVideo(video, playback.url, (message) => {
        setBuffering(false);
        setError(message);
      });
    } else {
      video.src = playback.url;
      video.load();
    }
    return () => {
      detach?.();
      video.pause(); video.removeAttribute('src'); video.load();
    };
  }, [playback?.url, playback?.mode]);

  const run = useCallback(async (label: string, task: (ticket: number) => Promise<void>) => {
    if (!desktop || busyRef.current) return;
    busyRef.current = true;
    const ticket = ++operation.current;
    setBusy(label);
    setError(null);
    try { await task(ticket); }
    catch (reason) { if (operation.current === ticket) setError(errorMessage(reason)); }
    finally {
      if (operation.current === ticket) { setBusy(null); busyRef.current = false; }
    }
  }, []);

  async function clearSource() {
    setPlayback(null);
    setBuffering(false);
    await invoke('stop_playback');
    if (torrent) await invoke('remove_torrent', { id: torrent.id });
    setTorrent(null);
    setLocalPath(null);
    setStats(null);
    setStatsError(null);
  }

  async function playTorrentFile(view: TorrentView, file: VideoFile, nextMode: PlaybackMode, ticket: number) {
    setPlayback(null);
    const data = await invoke<Playback>('select_file', { id: view.id, fileId: file.id, mode: nextMode });
    if (operation.current !== ticket) return;
    setTorrent({ ...view, selectedFile: file.id });
    setPlayback(data);
  }

  async function loadTorrent(command: 'add_magnet' | 'add_torrent_file', args: { magnet: string } | { path: string }, ticket: number) {
    await clearSource();
    const view = await invoke<TorrentView>(command, args);
    if (operation.current !== ticket) return;
    setTorrent(view);
    const firstFile = [...view.files].sort((a, b) => b.length - a.length)[0];
    if (!firstFile) throw new Error('Este torrent no contiene archivos de video compatibles con el selector.');
    await playTorrentFile(view, firstFile, mode, ticket);
    setMagnet('');
    setPanel(null);
  }

  function submitMagnet(event: FormEvent) {
    event.preventDefault();
    const value = magnet.trim();
    if (!value.toLowerCase().startsWith('magnet:?')) {
      setError('Pegá un enlace magnet válido, que empiece con magnet:?.');
      return;
    }
    void run('Conectando con peers y buscando metadatos…', (ticket) => loadTorrent('add_magnet', { magnet: value }, ticket));
  }

  function pickTorrent() {
    void run('Abriendo torrent…', async (ticket) => {
      const path = await open({ multiple: false, directory: false, filters: [{ name: 'Torrent', extensions: ['torrent'] }] });
      if (typeof path === 'string') await loadTorrent('add_torrent_file', { path }, ticket);
    });
  }

  function pickVideo() {
    void run('Preparando video local…', async (ticket) => {
      const path = await open({ multiple: false, directory: false, filters: [{ name: 'Video', extensions: ['mp4', 'm4v', 'mov', 'mkv', 'webm', 'avi', 'ts', 'm2ts', 'mpg', 'mpeg', 'ogv'] }] });
      if (typeof path !== 'string') return;
      await clearSource();
      setLocalPath(path);
      const data = await invoke<Playback>('open_local_video', { path, mode });
      if (operation.current === ticket) { setPlayback(data); setPanel(null); }
    });
  }

  function changeMode(next: PlaybackMode) {
    if (next === mode || disabled) return;
    setMode(next);
    if (!hasSource) return;
    void run('Preparando reproducción…', async (ticket) => {
      if (torrent && currentFile) await playTorrentFile(torrent, currentFile, next, ticket);
      else if (localPath) {
        setPlayback(null);
        const data = await invoke<Playback>('open_local_video', { path: localPath, mode: next });
        if (operation.current === ticket) setPlayback(data);
      }
    });
  }

  function mediaError() {
    setBuffering(false);
    setError(mode === 'direct'
      ? 'El reproductor no pudo abrir este video. Si el torrent tiene peers, probá el modo FFmpeg para convertir formatos no compatibles.'
      : 'No se pudo reproducir la salida de FFmpeg. Revisá que el archivo tenga video y que el torrent tenga peers disponibles.');
  }

  const togglePanel = (next: typeof panel) => setPanel((current) => current === next ? null : next);

  return (
    <div className={`app-shell${fullscreen ? ' is-fullscreen' : ''}${chromeHidden ? ' chrome-hidden' : ''}`}>
      <header className="app-header" inert={chromeHidden}>
        <div className="brand"><span className="brand-mark"><Play size={17} fill="currentColor" /></span><strong>mukure<span>.</span></strong></div>
        {sourceTitle && <div className="now-playing"><span>AHORA REPRODUCIENDO</span><strong title={sourceTitle}>{fileName(sourceTitle)}</strong></div>}
        <div className="header-actions">
          <button className={`header-button ${panel === 'open' ? 'active' : ''}`} onClick={() => togglePanel('open')} aria-label="Abrir video o torrent"><FolderOpen size={17} /><span>Abrir</span></button>
          <button className={`icon-button ${panel === 'cast' || casting.status?.active ? 'active' : ''}`} title="Enviar a Chromecast" aria-label="Enviar a Chromecast" onClick={() => togglePanel('cast')}><Cast size={19} /></button>
          <button className={`icon-button ${panel === 'providers' ? 'active' : ''}`} title="Fuentes de subtítulos" aria-label="Fuentes de subtítulos" onClick={() => togglePanel('providers')}><Settings2 size={18} /></button>
        </div>
      </header>
      {!desktop && <div className="preview-notice"><MonitorPlay size={15} />Vista previa. Abrí la app de escritorio para reproducir videos.</div>}
      {error && <div className="error-notice" role="alert"><span>{error}</span><button className="icon-button" aria-label="Cerrar error" onClick={() => setError(null)}><X size={16} /></button></div>}
      <main className={`player-workspace ${panel ? 'panel-open' : ''}`}>
        <section className="player-panel" aria-label="Reproductor de video">
          <div className={`video-stage ${playback ? 'has-video' : ''}`}>
            {playback ? <video style={casting.status?.active ? { visibility: 'hidden' } : undefined} key={playback.url} ref={videoRef} autoPlay playsInline preload="metadata" onClick={() => { const video = videoRef.current; if (video) { if (video.paused) void video.play().catch((reason: unknown) => setError(errorMessage(reason))); else video.pause(); } }} onWaiting={() => setBuffering(true)} onPlaying={() => { setBuffering(false); setMediaReady(true); }} onCanPlay={() => { setBuffering(false); setMediaReady(true); }} onLoadedMetadata={() => setMediaReady(true)} onError={playback.mode === 'compatible' ? undefined : mediaError} /> :
              <div className="empty-stage"><div className="empty-stage-content"><div className="stage-play">{busy ? <LoaderCircle size={32} className="spin" /> : <Play size={34} fill="currentColor" strokeWidth={1} />}</div><span className="eyebrow">TU CINE, A TU MANERA</span><h1>{busy ? 'Preparando tu video…' : 'Todo listo para darle play.'}</h1><p>{busy || 'Un archivo, un torrent o un magnet. Vos elegís.'}</p>{!busy && <div className="empty-actions"><button className="accent-button" onClick={pickVideo} disabled={disabled}><FolderOpen size={17} />Abrir video</button><button className="secondary-button" onClick={() => setPanel('open')}><Link2 size={16} />Magnet o torrent</button></div>}</div></div>}
            {playback && (busy || (buffering && !mediaReady)) && <div className="video-loading" role="status"><LoaderCircle className="spin" size={30} /><span>{busy || 'Esperando datos del video…'}</span></div>}
            {casting.status?.active && <div className="cast-stage"><Cast size={46} /><strong>{casting.status.deviceName}</strong><span>{casting.status.playerState === 'CONNECTING' ? 'Preparando transmisión…' : 'Reproduciendo en tu TV'}</span><button className="secondary-button" onClick={() => setPanel('cast')}>Ver transmisión</button></div>}
          </div>
          <PlayerControls videoRef={videoRef} playback={playback} onSubtitles={() => togglePanel('subtitles')} subtitlesOpen={panel === 'subtitles'} onError={setError} onFullscreenChange={setFullscreen} hidden={chromeHidden} cast={casting.status} onCastControl={casting.control} />
        </section>
        <aside className="drawer" hidden={!panel} aria-label="Opciones del reproductor">
          <div className="drawer-header"><div className="drawer-tabs"><button aria-label="Panel de Chromecast" title="Chromecast" className={panel === 'cast' ? 'active' : ''} onClick={() => setPanel('cast')}><Cast size={18} /></button><button aria-label="Panel de subtítulos" title="Subtítulos" className={panel === 'subtitles' ? 'active' : ''} onClick={() => setPanel('subtitles')}><Captions size={19} /></button><button aria-label="Panel de fuentes" title="Fuentes" className={panel === 'providers' ? 'active' : ''} onClick={() => setPanel('providers')}><KeyRound size={18} /></button><button aria-label="Archivos de la sesión" title="Archivos" className={panel === 'files' ? 'active' : ''} onClick={() => setPanel('files')}><ListVideo size={19} /></button><button aria-label="Ayuda del reproductor" title="Ayuda" className={panel === 'help' ? 'active' : ''} onClick={() => setPanel('help')}><CircleHelp size={18} /></button></div><button className="icon-button" aria-label="Cerrar panel" onClick={() => setPanel(null)}><X size={18} /></button></div>
          <div className="drawer-content"><div hidden={panel !== 'cast'}><CastPanel visible={panel === 'cast'} playback={playback} casting={casting} subtitle={castSubtitle} ffmpegAvailable={Boolean(info?.ffmpegAvailable)} /></div>
            <section hidden={panel !== 'open'} className="open-panel" aria-label="Abrir video"><div className="panel-heading"><FolderOpen size={20} /><div><h2>Abrir un video</h2><p>De tu equipo o desde un torrent.</p></div></div><button className="source-file-button" onClick={pickVideo} disabled={disabled}><FileVideo2 size={22} /><span><strong>Video local</strong><small>Elegí un archivo de tu equipo</small></span></button><button className="source-file-button" onClick={pickTorrent} disabled={disabled}><Radio size={22} /><span><strong>Archivo .torrent</strong><small>Reproducí mientras se descarga</small></span></button><form className="magnet-form" onSubmit={submitMagnet}><label htmlFor="magnet">O pegá un enlace magnet</label><textarea id="magnet" aria-label="Enlace magnet" placeholder="magnet:?xt=urn:btih:…" value={magnet} onChange={(event) => setMagnet(event.target.value)} disabled={disabled} spellCheck={false} /><button className="accent-button" type="submit" disabled={disabled || !magnet.trim()}><Play size={15} fill="currentColor" />Reproducir enlace</button></form></section>
            <div hidden={panel !== 'subtitles'}><SubtitleControls key={localPath ? `local:${localPath}` : torrent ? `torrent:${torrent.id}:${torrent.selectedFile}` : 'empty'} videoRef={videoRef} playback={playback} torrentId={torrent?.id} files={torrent?.subtitles || []} localPath={localPath} providers={providers.filter((p) => p.connected).map((p) => p.id)} onProviders={() => setPanel('providers')} onCastSubtitle={setCastSubtitle} disabled={disabled} /></div>
            <div hidden={panel !== 'providers'}><ProviderSettings statuses={providers} disabled={!desktop} onChange={refreshProviders} /></div>
            <section hidden={panel !== 'files'}><div className="panel-heading"><ListVideo size={20} /><div><h2>Sesión actual</h2><p>{hasSource ? torrent ? 'Elegí el video que querés reproducir.' : 'Video abierto desde tu equipo.' : 'Todavía no hay un video abierto.'}</p></div></div>{localPath && <div className="session-card"><FileVideo2 size={20} /><span>{fileName(localPath)}</span></div>}{torrent?.files.map((file) => <button className={`file-button ${torrent.selectedFile === file.id ? 'selected' : ''}`} key={file.id} disabled={disabled} onClick={() => void run('Cargando video…', (ticket) => playTorrentFile(torrent, file, mode, ticket))}><FileVideo2 size={18} /><span><strong>{fileName(file.name)}</strong><small>{formatBytes(file.length)}</small></span>{torrent.selectedFile === file.id && <Check size={15} />}</button>)}{torrent && <div className="transfer-panel"><div className="transfer-top"><span>{stats?.finished ? 'Descarga completa' : 'Descargando'}</span><strong>{progress.toFixed(1)}%</strong></div><div className="progress-track"><div style={{ width: `${progress}%` }} /></div><div className="transfer-details"><span><ArrowDown size={13} />{formatBytes(stats?.downloadSpeed || 0)}/s</span><span><ArrowUp size={13} />{formatBytes(stats?.uploadSpeed || 0)}/s</span><span><Users size={13} />{stats?.peers || 0} peers</span><span>{formatBytes(stats?.progressBytes || 0)} / {formatBytes(stats?.totalBytes || 0)}</span></div>{statsError && <p className="inline-error">{statsError}</p>}</div>}{hasSource && <button className="secondary-button close-session" disabled={disabled} onClick={() => void run('Cerrando sesión…', async () => clearSource())}><Square size={14} />Cerrar video y detener sesión</button>}</section>
            <section hidden={panel !== 'help'} className="help-panel"><div className="panel-heading"><CircleHelp size={20} /><div><h2>Acomodá tu reproductor</h2><p>El video ocupa el espacio disponible.</p></div></div><p>En pantalla completa, las barras y el cursor se ocultan tras 3 segundos de inactividad. Mové el mouse o usá el teclado para mostrarlos.</p><p>Arrastrá cualquier borde de la ventana para cambiar su tamaño.</p><dl className="shortcuts"><div><dt>Reproducir / pausar</dt><dd>Espacio</dd></div><div><dt>Saltar 10 segundos</dt><dd>← / →</dd></div><div><dt>Silenciar</dt><dd>M</dd></div><div><dt>Pantalla completa</dt><dd>F</dd></div></dl><p>Los subtítulos se buscan automáticamente al abrir un video. En el botón CC podés elegir una pista, cambiar el idioma y ajustar la sincronización.</p><p>Directo usa los formatos que admite tu equipo. FFmpeg convierte formatos adicionales; todavía no permite saltos y puede consumir más CPU.</p><p>Los torrents necesitan peers disponibles y también comparten piezas. Al cerrar la sesión se borran sus datos descargados.</p><p className="cache-info">Carpeta de datos:<br /><code>{info?.cacheDir || 'Disponible en la app de escritorio'}</code></p></section>
          </div>
        </aside>
      </main>
      <footer className="status-bar" inert={chromeHidden}><span className="playback-status"><span className={`status-dot ${playback ? 'is-active' : ''}`} />{busy ? 'Preparando…' : playback ? buffering ? 'Cargando datos…' : torrent ? 'Torrent' : 'Archivo local' : 'Listo para reproducir'}</span>{torrent && <button className="transfer-summary" onClick={() => setPanel('files')}><ArrowDown size={12} />{formatBytes(stats?.downloadSpeed || 0)}/s · {stats?.peers || 0} peers</button>}<div className="segmented-control" aria-label="Modo de reproducción"><button className={mode === 'direct' ? 'selected' : ''} aria-pressed={mode === 'direct'} disabled={disabled || Boolean(casting.status?.active)} onClick={() => changeMode('direct')}>Directo</button><button className={mode === 'compatible' ? 'selected' : ''} aria-pressed={mode === 'compatible'} disabled={disabled || Boolean(casting.status?.active) || !info?.ffmpegAvailable} title={info?.ffmpegAvailable ? 'Convertir con FFmpeg · sin saltos' : 'FFmpeg no disponible'} onClick={() => changeMode('compatible')}>FFmpeg</button></div></footer>
    </div>
  );
}

export default App;
