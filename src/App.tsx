import { useCallback, useEffect, useRef, useState, type FormEvent } from 'react';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import {
  Activity, ArrowDown, ArrowRight, ArrowUp, Check, ChevronRight, CircleHelp,
  Clapperboard, FileVideo2, FolderOpen, HardDrive, Link2, LoaderCircle,
  MonitorPlay, Play, Plus, Radio, ShieldCheck, Square, Users, X,
} from 'lucide-react';
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
  const [help, setHelp] = useState(false);
  const videoRef = useRef<HTMLVideoElement>(null);
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
      if (operation.current === ticket) setPlayback(data);
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

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <a className="brand" href="#" aria-label="Mukure, inicio" onClick={(event) => event.preventDefault()}>
          <span className="brand-mark"><Play size={19} fill="currentColor" strokeWidth={0} /></span>
          <span>MUKURE<span className="brand-dot">.</span></span>
        </a>
        <div className="sidebar-body">
          <div className="section-label">TU ESPACIO</div>
          <div className="nav-current"><MonitorPlay size={18} /><span>Reproductor</span><span className="nav-indicator" /></div>
          <div className="library-heading"><span className="section-label">SESIÓN ACTUAL</span><span className="count">{hasSource ? '1' : '0'}</span></div>
          {hasSource ? (
            <div className="session-card">
              <span className="session-icon"><FileVideo2 size={20} /></span>
              <div><strong title={sourceTitle || ''}>{sourceTitle ? fileName(sourceTitle) : 'Preparando video'}</strong><span>{torrent ? 'Torrent · sesión local' : 'Archivo local'}</span></div>
            </div>
          ) : (
            <div className="library-empty"><div className="library-empty-icon"><Clapperboard size={24} strokeWidth={1.4} /></div><p>Tu próxima película<br />empieza acá.</p><span>Agregá un enlace o un archivo.</span></div>
          )}
          {torrent && torrent.files.length > 0 && (
            <div className="torrent-files">
              <p className="section-label">ARCHIVOS DE VIDEO · {torrent.files.length}</p>
              {torrent.files.map((file) => (
                <button className={`file-button ${torrent.selectedFile === file.id ? 'selected' : ''}`} key={file.id} disabled={disabled} onClick={() => void run('Cargando video…', (ticket) => playTorrentFile(torrent, file, mode, ticket))} title={file.name}>
                  {torrent.selectedFile === file.id ? <Play size={13} fill="currentColor" /> : <FileVideo2 size={14} />}
                  <span><strong>{fileName(file.name)}</strong><small>{formatBytes(file.length)}</small></span>
                </button>
              ))}
            </div>
          )}
        </div>
        <button className={`help-button ${help ? 'active' : ''}`} onClick={() => setHelp(!help)}><CircleHelp size={17} />Cómo funciona<ChevronRight size={15} /></button>
        <div className="local-status"><span className="status-dot" /><div><strong>{desktop ? 'Todo en tu equipo' : 'Vista previa web'}</strong><span>{desktop ? `Mukure ${info?.version || '· escritorio'}` : 'La app funciona en escritorio'}</span></div><ShieldCheck size={18} /></div>
      </aside>

      <main className="main-content">
        <header className="page-header">
          <div><div className="eyebrow"><span /> TU CINE, SIN VUELTAS</div><h1>Le damos play<span>?</span></h1><p>Un enlace. Un video. Nada en el medio.</p></div>
          <span className="local-badge"><ShieldCheck size={14} />100% local</span>
        </header>

        {!desktop && <div className="preview-notice"><MonitorPlay size={17} /><span>Vista previa · abrí la app de escritorio para reproducir videos y conectar torrents.</span></div>}

        <section className="source-panel" aria-label="Abrir video">
          <form className="magnet-form" onSubmit={submitMagnet}>
            <Link2 size={20} className="input-icon" />
            <input aria-label="Enlace magnet" placeholder="Pegá tu magnet link acá…" value={magnet} onChange={(event) => setMagnet(event.target.value)} disabled={disabled} spellCheck={false} autoComplete="off" />
            <button className="primary-button" type="submit" disabled={disabled || !magnet.trim()}><Play size={14} fill="currentColor" />Reproducir<ArrowRight size={16} /></button>
          </form>
          <div className="source-actions"><span>o abrí un archivo</span><button onClick={pickTorrent} disabled={disabled}><Plus size={15} />Archivo .torrent</button><button onClick={pickVideo} disabled={disabled}><FolderOpen size={16} />Video local</button><span className="source-hint"><HardDrive size={13} />Sin cuentas. Sin nube.</span></div>
        </section>

        {error && <div className="error-notice" role="alert"><span>{error}</span><button aria-label="Cerrar error" onClick={() => setError(null)}><X size={16} /></button></div>}

        <section className="player-panel" aria-label="Reproductor de video">
          <div className="player-toolbar"><div className="player-title"><span className={`tiny-dot ${hasSource ? 'is-active' : ''}`} /><span title={sourceTitle || ''}>{sourceTitle ? fileName(sourceTitle) : 'Listo para una nueva historia'}</span></div><div className="player-toolbar-right">{hasSource && <button className="stop-button" disabled={disabled} onClick={() => void run('Cerrando sesión…', async () => clearSource())}><Square size={12} />Cerrar</button>}<span className="player-tag">{torrent ? 'TORRENT' : localPath ? 'LOCAL' : 'PLAYER'}</span></div></div>
          <div className={`video-stage ${playback ? 'has-video' : ''}`}>
            {playback ? (
              <video key={playback.url} ref={videoRef} controls autoPlay playsInline preload="metadata" onWaiting={() => setBuffering(true)} onPlaying={() => { setBuffering(false); setMediaReady(true); }} onCanPlay={() => { setBuffering(false); setMediaReady(true); }} onLoadedMetadata={() => setMediaReady(true)} onError={playback.mode === 'compatible' ? undefined : mediaError} />
            ) : (
              <div className="empty-stage"><div className="stage-grid" /><div className="stage-orbit orbit-one" /><div className="stage-orbit orbit-two" /><div className="stage-glow" /><div className="empty-stage-content"><div className="stage-play"><Play size={28} fill="currentColor" strokeWidth={0} /></div><h2>{busy ? 'Preparando tu video' : 'La pantalla es tuya.'}</h2><p>{busy || 'Traé algo bueno para ver.'}</p>{!busy && <span>MAGNET LINKS <i /> TORRENTS <i /> ARCHIVOS LOCALES</span>}</div><div className="stage-bottom"><span>MUKURE ORIGINAL PLAYER</span><span>PRESS PLAY. STAY LOCAL.</span></div></div>
            )}
            {playback && (busy || (buffering && !mediaReady)) && <div className="video-loading" role="status"><LoaderCircle className="spin" size={26} /><span>{busy || 'Esperando datos del video…'}</span></div>}
            {!playback && busy && <span className="stage-loading"><LoaderCircle className="spin" size={18} /></span>}
          </div>
          <div className="player-footer"><span><span className={`status-dot ${playback ? '' : 'muted'}`} />{busy ? 'Preparando' : playback ? (buffering ? 'Cargando datos' : 'Video abierto') : 'Esperando un video'}</span><span><MonitorPlay size={14} />{mode === 'direct' ? 'Reproducción directa' : 'Compatibilidad FFmpeg'}</span></div>
        </section>

        <section className="below-player">
          <div className="playback-mode"><div className="mode-heading"><Activity size={17} /><div><strong>A tu ritmo, a tu manera.</strong><p>{mode === 'direct' ? 'Usá el decodificador del sistema, sin convertir el video.' : 'FFmpeg convierte a H.264 y AAC mientras reproduce. Puede usar más CPU.'}</p></div></div><div className="segmented-control" aria-label="Modo de reproducción"><button className={mode === 'direct' ? 'selected' : ''} aria-pressed={mode === 'direct'} disabled={disabled} onClick={() => changeMode('direct')}>{mode === 'direct' && <Check size={13} />}Directo</button><button className={mode === 'compatible' ? 'selected' : ''} aria-pressed={mode === 'compatible'} disabled={disabled || !info?.ffmpegAvailable} title={info?.ffmpegAvailable ? 'Convertir con FFmpeg' : 'FFmpeg no está disponible en esta instalación'} onClick={() => changeMode('compatible')}>{mode === 'compatible' && <Check size={13} />}FFmpeg</button></div></div>
          {mode === 'compatible' && <p className="compat-note">La conversión empieza desde cero. Adelantar y retroceder todavía no están disponibles en este modo.</p>}
          <SubtitleControls key={localPath ? `local:${localPath}` : torrent ? `torrent:${torrent.id}:${torrent.selectedFile}` : 'empty'} videoRef={videoRef} playback={playback} torrentId={torrent?.id} files={torrent?.subtitles || []} disabled={disabled} />
          {torrent && <div className="transfer-panel"><div className="transfer-top"><span><Radio size={14} />{stats?.finished ? 'Descarga completa · compartiendo' : stats?.state || 'Conectando con peers'}</span><strong>{progress.toLocaleString('es', { maximumFractionDigits: 1 })}%</strong></div><div className="progress-track"><div style={{ width: `${progress}%` }} /></div><div className="transfer-details"><span><ArrowDown size={13} />{formatBytes(stats?.downloadSpeed || 0)}/s</span><span><ArrowUp size={13} />{formatBytes(stats?.uploadSpeed || 0)}/s</span><span><Users size={13} />{stats?.peers || 0} peers</span><span>{formatBytes(stats?.progressBytes || 0)} / {formatBytes(stats?.totalBytes || 0)}</span></div>{statsError && <p className="stats-error">No se pudo actualizar el estado: {statsError}</p>}</div>}
        </section>

        {help && <section className="help-panel" aria-label="Cómo funciona"><div className="help-heading"><h2>Simple por fuera. Local por dentro.</h2><button aria-label="Cerrar ayuda" onClick={() => setHelp(false)}><X size={17} /></button></div><div className="help-grid"><div><Link2 size={19} /><h3>Conectá tu video</h3><p>Pegá un magnet, abrí un .torrent o elegí un archivo local. El torrent necesita peers disponibles para descargar.</p></div><div><MonitorPlay size={19} /><h3>Miralo mientras llega</h3><p>Elegí un archivo de video en la sesión. Los datos se descargan a tu equipo y se entregan al player por un puerto local.</p></div><div><ShieldCheck size={19} /><h3>Vos tenés el control</h3><p>No hay cuentas ni servidor remoto. Los torrents comparten datos con otros peers. Al cerrar la sesión se detiene la conexión.</p></div></div><div className="cache-info"><HardDrive size={14} /><span>Carpeta de datos: <code>{info?.cacheDir || 'Disponible al abrir la app de escritorio'}</code></span></div>{!info?.ffmpegAvailable && <p className="ffmpeg-help">El modo FFmpeg se habilita cuando la app encuentra el ejecutable FFmpeg. La reproducción directa depende de los formatos que soporte el sistema.</p>}</section>}

        <footer className="page-footer"><span>Menos plataforma. Más película.</span><span>Hecho para darle play <Play size={10} fill="currentColor" /></span></footer>
      </main>
    </div>
  );
}

export default App;
