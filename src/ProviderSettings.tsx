import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Check, ExternalLink, KeyRound, LoaderCircle, Unplug } from 'lucide-react';
import { errorMessage } from './format';

export interface ProviderStatus { id: string; connected: boolean }
const providers = [
  { id: 'subdl', name: 'SubDL', quota: '2.000 consultas gratis por día.', steps: 'Creá tu cuenta en SubDL. En el panel API, generá una clave y pegala acá.', note: 'Las descargas anónimas tienen un cupo de 300 por día y por IP.' },
  { id: 'wyzie', name: 'Wyzie', quota: '1.000 consultas gratis por día.', steps: 'Ingresá tu email, completá la verificación y copiá la clave que empieza con wyzie-.', note: 'Cada búsqueda y cada descarga consumen una consulta. El plan gratuito agrega un aviso al inicio del subtítulo.' },
];
export default function ProviderSettings({ statuses, disabled, onChange }: { statuses: ProviderStatus[]; disabled: boolean; onChange: () => Promise<void> }) {
  const [keys, setKeys] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState<string | null>(null);
  const [messages, setMessages] = useState<Record<string, { text: string; error: boolean }>>({});
  async function connect(id: string) {
    setBusy(id);
    setMessages((old) => ({ ...old, [id]: { text: 'Comprobando la conexión…', error: false } }));
    try {
      await invoke('connect_subtitle_provider', { provider: id, apiKey: keys[id]?.trim() || null });
      setKeys((old) => ({ ...old, [id]: '' }));
      await onChange();
      setMessages((old) => ({ ...old, [id]: { text: 'Conexión verificada. Se usará en las próximas búsquedas.', error: false } }));
    } catch (error) { setMessages((old) => ({ ...old, [id]: { text: errorMessage(error), error: true } })); }
    finally { setBusy(null); }
  }
  async function disconnect(id: string) {
    setBusy(id);
    try {
      await invoke('disconnect_subtitle_provider', { provider: id });
      await onChange();
      setMessages((old) => ({ ...old, [id]: { text: 'Desconectado. Clave eliminada del Llavero.', error: false } }));
    } catch (error) { setMessages((old) => ({ ...old, [id]: { text: errorMessage(error), error: true } })); }
    finally { setBusy(null); }
  }
  async function openPage(id: string, docs = false) {
    try { await invoke('open_subtitle_provider_page', { provider: `${id}${docs ? '-docs' : ''}` }); }
    catch (error) { setMessages((old) => ({ ...old, [id]: { text: errorMessage(error), error: true } })); }
  }
  return <section className="provider-settings" aria-label="Fuentes de subtítulos">
    <div className="panel-heading"><KeyRound size={19} /><div><h2>Conectá más fuentes</h2><p>Opcionales. OpenSubtitles y SubtitleDB ya funcionan sin cuenta.</p></div></div>
    <div className="provider-grid">{providers.map((provider) => {
      const connected = statuses.some((s) => s.id === provider.id && s.connected);
      const message = messages[provider.id];
      return <article className="provider-card" key={provider.id}>
        <div className="provider-title"><h3>{provider.name}</h3><span className={connected ? 'connected badge' : 'badge'}>{connected ? <><Check size={12} />Conectado</> : 'Gratis con registro'}</span></div>
        <strong className="provider-quota">{provider.quota}</strong><p>{provider.steps}</p>
        <div className="provider-links"><button disabled={disabled} onClick={() => void openPage(provider.id)}><ExternalLink size={13} />Obtener clave gratis</button><button disabled={disabled} onClick={() => void openPage(provider.id, true)}>Documentación y límites</button></div>
        <label className="key-field">Clave de API<input type="password" autoComplete="off" spellCheck={false} aria-label={`Clave API de ${provider.name}`} placeholder={connected ? 'Clave guardada · pegar para reemplazar' : 'Pegá tu clave aquí'} value={keys[provider.id] || ''} disabled={disabled || Boolean(busy)} onChange={(event) => setKeys((old) => ({ ...old, [provider.id]: event.target.value }))} /></label>
        <div className="provider-buttons"><button className="accent-button" disabled={disabled || Boolean(busy) || (!connected && !keys[provider.id]?.trim())} onClick={() => void connect(provider.id)}>{busy === provider.id ? <LoaderCircle size={14} className="spin" /> : <KeyRound size={14} />}{connected && !keys[provider.id]?.trim() ? 'Probar conexión' : 'Probar y conectar'}</button>{connected && <button className="plain-button" disabled={Boolean(busy)} onClick={() => void disconnect(provider.id)}><Unplug size={14} />Desconectar</button>}</div>
        {message && <p className={message.error ? 'inline-error' : 'inline-success'} role={message.error ? 'alert' : 'status'}>{message.text}</p>}
        <small>{provider.note}</small>
      </article>;
    })}</div>
    <p className="panel-note">Las claves se guardan en el Llavero de macOS. Probar una conexión puede consumir una consulta. Los cupos dependen de cada proveedor y pueden cambiar.</p>
  </section>;
}
