import { useCallback, useEffect, useRef, useState, type RefObject } from 'react';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { errorMessage } from './format';
import type { CastAction, CastStatus, Playback } from './types';

export function useCasting(videoRef: RefObject<HTMLVideoElement | null>, playback: Playback | null) {
  const [status, setStatus] = useState<CastStatus | null>(null);
  const [connecting, setConnecting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const currentPlayback = useRef(playback);
  currentPlayback.current = playback;
  const timers = useRef<Partial<Record<CastAction, ReturnType<typeof setTimeout>>>>({});
  const pending = useRef(Promise.resolve());
  const busy = useRef(false);
  const generation = useRef(0);

  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      try {
        const next = await invoke<CastStatus>('get_cast_status');
        if (!cancelled) setStatus(next);
      } catch (reason) { if (!cancelled) setError(errorMessage(reason)); }
      finally { if (!cancelled) timer = setTimeout(poll, 1000); }
    }
    void poll();
    return () => { cancelled = true; clearTimeout(timer); };
  }, []);

  useEffect(() => {
    const video = videoRef.current;
    if (!video || !status?.active) return;
    video.pause();
    const pauseLocal = () => video.pause();
    video.addEventListener('play', pauseLocal);
    return () => video.removeEventListener('play', pauseLocal);
  }, [videoRef, status?.active, playback?.url]);

  useEffect(() => {
    ++generation.current;
    Object.values(timers.current).forEach(clearTimeout);
    return () => { ++generation.current; Object.values(timers.current).forEach(clearTimeout); };
  }, [playback?.url]);

  const control = useCallback((action: CastAction, value?: number) => {
    clearTimeout(timers.current[action]);
    const ticket = generation.current;
    const send = () => {
      pending.current = pending.current.then(async () => {
        if (ticket !== generation.current) return;
        await invoke('cast_action', { action, value: value ?? null });
        const next = await invoke<CastStatus>('get_cast_status');
        if (ticket === generation.current) setStatus(next);
      }).catch((reason: unknown) => { if (ticket === generation.current) setError(errorMessage(reason)); });
    };
    if (action === 'seek' || action === 'volume') timers.current[action] = setTimeout(send, 200);
    else send();
  }, []);

  async function start(deviceId: string | null, address: string | null, convert: boolean, subtitle: string | null) {
    if (!playback || busy.current) return;
    ++generation.current;
    busy.current = true; setConnecting(true); setError(null);
    const ticket = generation.current;
    const video = videoRef.current;
    const wasPlaying = video && !video.paused;
    const position = video?.currentTime || 0;
    video?.pause();
    try {
      const next = await invoke<CastStatus>('start_cast', { playbackUrl: playback.url, deviceId, address,
        title: playback.name, position: Number.isFinite(position) ? position : 0, convert, subtitle });
      if (ticket === generation.current && currentPlayback.current?.url === playback.url) setStatus(next);
    } catch (reason) {
      if (ticket !== generation.current) return;
      setError(errorMessage(reason));
      if (wasPlaying && video && currentPlayback.current?.url === playback.url) void video.play().catch(() => {});
    } finally { busy.current = false; setConnecting(false); }
  }
  async function stop() {
    ++generation.current;
    setError(null);
    Object.values(timers.current).forEach(clearTimeout);
    try {
      await invoke('stop_cast');
      const video = videoRef.current;
      if (video && playback?.mode === 'direct' && status?.canSeek) video.currentTime = status.currentTime;
      setStatus(null);
    } catch (reason) { setError(errorMessage(reason)); }
  }
  return { status, connecting, error: error || status?.error, control, start, stop };
}
