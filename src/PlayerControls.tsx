import { useCallback, useEffect, useState, type RefObject } from 'react';
import { isTauri } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { Captions, Maximize, Minimize, Pause, Play, RotateCcw, RotateCw, Volume2, VolumeX } from 'lucide-react';
import type { CastAction, CastStatus, Playback } from './types';
import { errorMessage } from './format';

export function formatTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '0:00';
  const value = Math.floor(seconds);
  const h = Math.floor(value / 3600);
  return `${h ? `${h}:` : ''}${h ? String(Math.floor(value / 60) % 60).padStart(2, '0') : Math.floor(value / 60)}:${String(value % 60).padStart(2, '0')}`;
}

export default function PlayerControls({ videoRef, playback, onSubtitles, subtitlesOpen, onError, onFullscreenChange, hidden, cast, onCastControl }: {
  videoRef: RefObject<HTMLVideoElement | null>; playback: Playback | null;
  onSubtitles: () => void; subtitlesOpen: boolean; onError: (message: string) => void;
  onFullscreenChange: (fullscreen: boolean) => void; hidden: boolean;
  cast: CastStatus | null; onCastControl: (action: CastAction, value?: number) => void;
}) {
  const [paused, setPaused] = useState(true);
  const [position, setPosition] = useState(0);
  const [duration, setDuration] = useState(0);
  const [volume, setVolume] = useState(1);
  const [muted, setMuted] = useState(false);
  const [rate, setRate] = useState(1);
  const [fullscreen, setFullscreen] = useState(false);
  const remote = Boolean(cast?.active);
  const shownPaused = remote ? cast?.playerState !== 'PLAYING' : paused;
  const shownPosition = remote ? cast?.currentTime || 0 : position;
  const shownDuration = remote ? cast?.duration || 0 : duration;
  const shownVolume = remote ? cast?.volume ?? 0.5 : volume;
  const shownMuted = remote ? Boolean(cast?.muted) : muted;
  const canSeek = remote ? Boolean(cast?.canSeek && shownDuration > 0 && cast.playerState !== 'CONNECTING') : Boolean(playback && playback.mode === 'direct' && duration > 0);

  useEffect(() => { onFullscreenChange(fullscreen); }, [fullscreen, onFullscreenChange]);

  useEffect(() => {
    const video = videoRef.current;
    setPosition(0); setDuration(0); setPaused(true);
    if (!video || !playback) return;
    video.volume = volume; video.muted = muted; video.playbackRate = rate;
    const update = () => {
      setPaused(video.paused); setPosition(video.currentTime);
      setDuration(Number.isFinite(video.duration) ? video.duration : 0);
      setVolume(video.volume); setMuted(video.muted); setRate(video.playbackRate);
    };
    const events = ['timeupdate', 'durationchange', 'loadedmetadata', 'play', 'pause', 'ended', 'volumechange', 'ratechange'];
    events.forEach((name) => video.addEventListener(name, update));
    update();
    return () => events.forEach((name) => video.removeEventListener(name, update));
  }, [videoRef, playback?.url]);

  const togglePlay = useCallback(() => {
    if (remote) { onCastControl(shownPaused ? 'play' : 'pause'); return; }
    const video = videoRef.current;
    if (!video || !playback) return;
    if (video.paused) void video.play().catch((reason: unknown) => onError(errorMessage(reason)));
    else video.pause();
  }, [videoRef, playback, onError, remote, shownPaused, onCastControl]);
  const seek = useCallback((value: number) => {
    if (!canSeek) return;
    const next = Math.max(0, Math.min(shownDuration, value));
    if (remote) onCastControl('seek', next);
    else if (videoRef.current) videoRef.current.currentTime = next;
  }, [canSeek, shownDuration, videoRef, remote, onCastControl]);
  const toggleFullscreen = useCallback(async () => {
    try {
      if (isTauri()) {
        const window = getCurrentWindow();
        const next = !(await window.isFullscreen());
        await window.setFullscreen(next); setFullscreen(next);
      } else if (document.fullscreenElement) await document.exitFullscreen();
      else await document.documentElement.requestFullscreen();
    } catch (reason) { onError(errorMessage(reason)); }
  }, [onError]);
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    if (isTauri()) {
      const window = getCurrentWindow();
      const update = () => { void window.isFullscreen().then((value) => { if (!cancelled) setFullscreen(value); }).catch(() => {}); };
      update();
      void window.onResized(update).then((stop) => { if (cancelled) stop(); else unlisten = stop; });
    }
    const changed = () => setFullscreen(Boolean(document.fullscreenElement));
    document.addEventListener('fullscreenchange', changed);
    return () => { cancelled = true; unlisten?.(); document.removeEventListener('fullscreenchange', changed); };
  }, []);
  useEffect(() => {
    const keydown = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement;
      if (!playback || event.altKey || event.metaKey || event.ctrlKey || target.closest('input,textarea,select,button,a,[contenteditable="true"]')) return;
      if (event.code === 'Space') { event.preventDefault(); togglePlay(); }
      else if (event.code === 'ArrowRight') { event.preventDefault(); seek((remote ? shownPosition : videoRef.current?.currentTime || 0) + 10); }
      else if (event.code === 'ArrowLeft') { event.preventDefault(); seek((remote ? shownPosition : videoRef.current?.currentTime || 0) - 10); }
      else if (event.key.toLowerCase() === 'm') { if (remote) onCastControl('mute', shownMuted ? 0 : 1); else if (videoRef.current) videoRef.current.muted = !videoRef.current.muted; }
      else if (event.key.toLowerCase() === 'f') void toggleFullscreen();
    };
    window.addEventListener('keydown', keydown);
    return () => window.removeEventListener('keydown', keydown);
  }, [playback, togglePlay, seek, toggleFullscreen, videoRef, remote, shownPosition, shownMuted, onCastControl]);

  return <div className="transport" aria-label="Controles de reproducción" inert={hidden}>
    <div className="timeline"><span>{formatTime(shownPosition)}</span><input aria-label="Posición del video" type="range" min={0} max={shownDuration || 1} step={0.1} value={Math.min(shownPosition, shownDuration || 1)} disabled={!canSeek} title={remote && !cast?.canSeek || playback?.mode === 'compatible' ? 'El modo FFmpeg todavía no permite saltos' : 'Buscar una posición'} onChange={(event) => seek(Number(event.target.value))} style={{ '--progress': `${shownDuration ? shownPosition / shownDuration * 100 : 0}%` } as React.CSSProperties} /><span>{shownDuration ? formatTime(shownDuration) : '–:––'}</span></div>
    <div className="transport-row">
      <div className="transport-group">
        <button className="play-button" aria-label={shownPaused ? 'Reproducir' : 'Pausar'} title="Reproducir / pausar (Espacio)" disabled={!playback || (remote && cast?.playerState === 'CONNECTING')} onClick={togglePlay}>{shownPaused ? <Play size={21} fill="currentColor" /> : <Pause size={21} fill="currentColor" />}</button>
        <button className="icon-button skip-button" aria-label="Retroceder 10 segundos" title="Retroceder 10 segundos (←)" disabled={!canSeek} onClick={() => seek(shownPosition - 10)}><RotateCcw size={19} /><span>10</span></button>
        <button className="icon-button skip-button" aria-label="Adelantar 10 segundos" title="Adelantar 10 segundos (→)" disabled={!canSeek} onClick={() => seek(shownPosition + 10)}><RotateCw size={19} /><span>10</span></button>
        <div className="volume-control"><button className="icon-button" aria-label={shownMuted ? 'Activar sonido' : 'Silenciar'} title="Silenciar (M)" disabled={!playback || (remote && cast?.playerState === 'CONNECTING')} onClick={() => { if (remote) onCastControl('mute', shownMuted ? 0 : 1); else if (videoRef.current) videoRef.current.muted = !shownMuted; }}>{shownMuted || shownVolume === 0 ? <VolumeX size={20} /> : <Volume2 size={20} />}</button><input aria-label="Volumen" type="range" min={0} max={1} step={0.05} value={shownMuted ? 0 : shownVolume} disabled={!playback || (remote && cast?.playerState === 'CONNECTING')} onChange={(event) => { if (remote) onCastControl('volume', Number(event.target.value)); else if (videoRef.current) { videoRef.current.volume = Number(event.target.value); videoRef.current.muted = false; } }} /></div>
      </div>
      <div className="transport-group"><select className="speed-select" aria-label="Velocidad de reproducción" disabled={!playback || remote} value={rate} onChange={(event) => { if (videoRef.current) videoRef.current.playbackRate = Number(event.target.value); }}>{[0.5, 0.75, 1, 1.25, 1.5, 2].map((value) => <option key={value} value={value}>{value}×</option>)}</select><button className={`icon-button ${subtitlesOpen ? 'active' : ''}`} aria-label="Subtítulos" title="Subtítulos" aria-pressed={subtitlesOpen} onClick={onSubtitles}><Captions size={22} /></button><button className="icon-button" aria-label={fullscreen ? 'Salir de pantalla completa' : 'Pantalla completa'} title="Pantalla completa (F)" onClick={() => void toggleFullscreen()}>{fullscreen ? <Minimize size={20} /> : <Maximize size={20} />}</button></div>
    </div>
  </div>;
}
