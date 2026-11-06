import { useEffect, useRef, useState } from 'react';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { Cast, LoaderCircle, MonitorPlay, RefreshCw, Square, Wifi } from 'lucide-react';
import type { CastDevice, Playback } from './types';
import type { useCasting } from './useCasting';
import { errorMessage } from './format';

export default function CastPanel({ visible, playback, casting, subtitle, ffmpegAvailable }: {
  visible: boolean; playback: Playback | null; casting: ReturnType<typeof useCasting>;
  subtitle: string | null; ffmpegAvailable: boolean;
}) {
  const [devices, setDevices] = useState<CastDevice[]>([]);
  const [searching, setSearching] = useState(false);
  const [searched, setSearched] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [address, setAddress] = useState('');
  const [convert, setConvert] = useState(false);
  const scan = useRef(false);
  const active = casting.status?.active || casting.connecting;
  useEffect(() => {
    setConvert(Boolean(playback && ffmpegAvailable && (playback.mode === 'compatible' || !/\.(mp4|m4v|webm)$/i.test(playback.name))));
  }, [playback?.url, ffmpegAvailable]);
  async function discover() {
    if (!isTauri() || scan.current) return;
    scan.current = true; setSearching(true); setError(null);
    try { setDevices(await invoke<CastDevice[]>('discover_cast_devices')); }
    catch (reason) { setError(errorMessage(reason)); }
    finally { scan.current = false; setSearching(false); setSearched(true); }
  }
  useEffect(() => { if (visible && !searched) void discover(); }, [visible]);
  return <section className="cast-panel" aria-label="Transmitir a Chromecast">
    <div className="panel-heading"><Cast size={21} /><div><h2>Enviar a Chromecast</h2><p>Tu video, en la pantalla grande.</p></div></div>
    <p className="panel-note">Conectá el Chromecast y esta Mac a la misma red. Mantené Mukure abierto mientras mirás.</p>
    {active ? <div className="cast-current"><Cast size={27} /><strong>{casting.status?.deviceName || 'Conectando…'}</strong><span>{casting.connecting ? convert ? 'Preparando video para TV…' : 'Conectando al dispositivo…' : casting.status?.playerState === 'PAUSED' ? 'En pausa' : casting.status?.playerState === 'BUFFERING' ? 'Cargando video…' : 'Transmitiendo'}</span><button className="secondary-button" onClick={() => void casting.stop()}><Square size={14} />Detener transmisión</button><p>La barra del reproductor controla la TV. Al detener, el video queda pausado en esta Mac.</p></div> : <>
      <label className="subtitle-toggle cast-convert"><input type="checkbox" checked={convert} disabled={!ffmpegAvailable} onChange={(event) => setConvert(event.target.checked)} />Convertir para TV</label>
      <p className="panel-note">{convert ? 'Convierte a H.264 y AAC, hasta 1080p. Empieza desde el inicio y no permite saltos.' : 'Envía el archivo original desde la posición actual. Si el Chromecast no admite sus códecs, activá la conversión.'}</p>
      <p className="panel-note">{subtitle ? 'Se enviará una copia de los subtítulos externos seleccionados con su ajuste actual.' : 'Podés elegir subtítulos externos en CC antes de transmitir. Las pistas integradas se muestran solo en esta Mac.'}</p>
      <button className="secondary-button cast-refresh" disabled={searching || !isTauri()} onClick={() => void discover()}>{searching ? <LoaderCircle size={15} className="spin" /> : <RefreshCw size={15} />}{searching ? 'Buscando dispositivos…' : 'Buscar dispositivos'}</button>
      {devices.map((device) => <button className="source-file-button" key={device.id} disabled={!playback} onClick={() => void casting.start(device.id, null, convert, subtitle)}><MonitorPlay size={23} /><span><strong>{device.name}</strong><small>{device.model} · {device.address}</small></span><Cast size={17} /></button>)}
      {searched && !searching && devices.length === 0 && <p className="panel-note">No se encontraron dispositivos. Revisá que el Chromecast esté encendido y que Mukure tenga permiso de red local en Ajustes del Sistema. Una VPN o una red de invitados puede impedir la conexión.</p>}
      {!playback && <p className="panel-note">Abrí un video para transmitir.</p>}
      <details className="cast-manual"><summary>Conectar por dirección IP</summary><form onSubmit={(event) => { event.preventDefault(); void casting.start(null, address.trim(), convert, subtitle); }}><label>IP del Chromecast<input aria-label="Dirección IP del Chromecast" placeholder="192.168.1.20" value={address} onChange={(event) => setAddress(event.target.value)} spellCheck={false} autoComplete="off" /></label><button className="accent-button" disabled={!playback || !address.trim()} type="submit"><Wifi size={14} />Conectar</button></form><p className="panel-note">Podés consultar la IP en los ajustes del dispositivo en Google Home.</p></details>
    </>}
    {(error || casting.error) && <p className="inline-error" role="alert">{error || casting.error}</p>}
  </section>;
}
