use crate::{
    cast_protocol::{CastClient, CONNECTION, HEARTBEAT, MEDIA, RECEIVER},
    media::MediaServer,
};
use anyhow::{bail, ensure, Context, Result};
use mdns_sd::{ServiceDaemon, ServiceEvent};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::UdpSocket,
    sync::{mpsc, oneshot, Mutex, RwLock},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CastDevice {
    pub id: String,
    pub name: String,
    pub model: String,
    pub address: Ipv4Addr,
    pub port: u16,
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CastStatus {
    pub active: bool,
    pub device_name: String,
    pub player_state: String,
    pub current_time: f64,
    pub duration: Option<f64>,
    pub volume: Option<f64>,
    pub muted: bool,
    pub can_seek: bool,
    pub error: Option<String>,
}
struct Command {
    action: String,
    value: Option<f64>,
    reply: oneshot::Sender<Result<()>>,
}
struct Session {
    cancel: CancellationToken,
    task: JoinHandle<()>,
    commands: mpsc::Sender<Command>,
    status: Arc<RwLock<CastStatus>>,
}
#[derive(Default)]
pub struct CastManager {
    devices: Mutex<HashMap<String, CastDevice>>,
    session: Mutex<Option<Session>>,
    discovery: Mutex<()>,
}
fn lan_address(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_link_local()
}

struct Discovery(ServiceDaemon);
impl Drop for Discovery {
    fn drop(&mut self) {
        let _ = self.0.shutdown();
    }
}
impl CastManager {
    pub async fn discover(&self) -> Result<Vec<CastDevice>> {
        let _guard = self
            .discovery
            .try_lock()
            .context("Ya hay una búsqueda de dispositivos en curso")?;
        let daemon = Discovery(
            ServiceDaemon::new()
                .context("No se pudo buscar Chromecast. Revisá el permiso de red local de macOS")?,
        );
        let events = daemon.0.browse("_googlecast._tcp.local.")?;
        let deadline = tokio::time::sleep(Duration::from_secs(5));
        tokio::pin!(deadline);
        let mut found = HashMap::new();
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                event = events.recv_async() => match event {
                    Ok(ServiceEvent::ServiceResolved(info)) => {
                        if info.get_property_val_str("ca").and_then(|v| v.parse::<u64>().ok()).is_some_and(|v| v & 1 == 0) { continue; }
                        if let Some(address) = info.get_addresses_v4().into_iter().find(|ip| lan_address(*ip)) {
                            let device = CastDevice { id: info.fullname.clone(),
                                name: info.get_property_val_str("fn").unwrap_or("Chromecast").chars().take(100).collect(),
                                model: info.get_property_val_str("md").unwrap_or("Google Cast").chars().take(100).collect(),
                                address, port: info.port };
                            found.insert(device.id.clone(), device);
                        }
                    },
                    Err(_) => break,
                    _ => {}
                }
            }
        }
        let mut devices: Vec<_> = found.values().cloned().collect();
        devices.sort_by(|a, b| a.name.cmp(&b.name));
        *self.devices.lock().await = found;
        Ok(devices)
    }
    pub async fn status(&self) -> CastStatus {
        let state = self.session.lock().await.as_ref().map(|s| s.status.clone());
        match state {
            Some(s) => s.read().await.clone(),
            None => CastStatus::default(),
        }
    }
    pub async fn start(
        &self,
        media: Arc<MediaServer>,
        playback_url: String,
        device_id: Option<String>,
        address: Option<String>,
        title: String,
        position: f64,
        convert: bool,
        subtitle: Option<String>,
    ) -> Result<CastStatus> {
        ensure!(
            position.is_finite() && (0.0..=604800.0).contains(&position),
            "Posición de reproducción inválida"
        );
        ensure!(title.len() <= 1024, "Nombre de video demasiado largo");
        let device = if let Some(address) = address.filter(|s| !s.trim().is_empty()) {
            let ip: Ipv4Addr = address
                .trim()
                .parse()
                .context("Ingresá una dirección IPv4 local, por ejemplo 192.168.1.20")?;
            ensure!(
                lan_address(ip),
                "El dispositivo debe estar en una dirección privada de tu red local"
            );
            CastDevice {
                id: format!("manual:{ip}"),
                name: format!("Chromecast · {ip}"),
                model: "Google Cast".into(),
                address: ip,
                port: 8009,
            }
        } else {
            self.devices
                .lock()
                .await
                .get(device_id.as_deref().unwrap_or(""))
                .cloned()
                .context("Buscá y elegí un Chromecast primero")?
        };
        let mut active = self.session.lock().await;
        ensure!(
            active.as_ref().is_none_or(|s| s.cancel.is_cancelled()),
            "Detené la transmisión actual antes de elegir otro dispositivo"
        );
        let cancel = CancellationToken::new();
        let (commands, receiver) = mpsc::channel(8);
        let status = Arc::new(RwLock::new(CastStatus {
            active: true,
            device_name: device.name.clone(),
            player_state: "CONNECTING".into(),
            can_seek: !convert,
            ..Default::default()
        }));
        let (ready, started) = oneshot::channel();
        let work_status = status.clone();
        let work_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            let mut ready = Some(ready);
            let result = run_session(
                media,
                &playback_url,
                device,
                title,
                position,
                convert,
                subtitle,
                receiver,
                work_status.clone(),
                work_cancel.clone(),
                &mut ready,
            )
            .await;
            work_cancel.cancel();
            let mut state = work_status.write().await;
            state.active = false;
            state.player_state = "IDLE".into();
            if let Err(error) = &result {
                state.error = Some(error.to_string());
            }
            if let Some(sender) = ready {
                let _ = sender.send(result.map(|_| state.clone()));
            }
        });
        *active = Some(Session {
            cancel: cancel.clone(),
            task,
            commands,
            status,
        });
        drop(active);
        match tokio::time::timeout(Duration::from_secs(100), started).await {
            Ok(Ok(result)) => result,
            _ => {
                cancel.cancel();
                bail!("No se pudo iniciar la transmisión a tiempo")
            }
        }
    }
    pub async fn action(&self, action: String, value: Option<f64>) -> Result<()> {
        ensure!(
            matches!(
                action.as_str(),
                "play" | "pause" | "seek" | "volume" | "mute"
            ),
            "Comando Cast desconocido"
        );
        if matches!(action.as_str(), "seek" | "volume" | "mute") {
            let value = value.context("Falta el valor del control")?;
            ensure!(
                value.is_finite()
                    && value >= 0.0
                    && value <= if action == "seek" { 604800.0 } else { 1.0 },
                "Valor de control inválido"
            );
        }
        let commands = self
            .session
            .lock()
            .await
            .as_ref()
            .filter(|s| !s.cancel.is_cancelled())
            .map(|s| s.commands.clone())
            .context("No hay una transmisión activa")?;
        let (reply, response) = oneshot::channel();
        commands
            .try_send(Command {
                action,
                value,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("Esperá a que termine el control anterior"))?;
        tokio::time::timeout(Duration::from_secs(30), response)
            .await
            .context("El Chromecast no respondió al control")?
            .context("La transmisión se detuvo")?
    }
    pub async fn stop(&self) {
        if let Some(mut session) = self.session.lock().await.take() {
            session.cancel.cancel();
            if tokio::time::timeout(Duration::from_secs(2), &mut session.task)
                .await
                .is_err()
            {
                session.task.abort();
                let _ = session.task.await;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_session(
    media: Arc<MediaServer>,
    playback_url: &str,
    device: CastDevice,
    title: String,
    position: f64,
    convert: bool,
    subtitle: Option<String>,
    mut commands: mpsc::Receiver<Command>,
    status: Arc<RwLock<CastStatus>>,
    cancel: CancellationToken,
    ready: &mut Option<oneshot::Sender<Result<CastStatus>>>,
) -> Result<()> {
    // UDP connect selects the interface to this receiver without sending data.
    let route = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    route.connect((device.address, device.port)).await?;
    let std::net::IpAddr::V4(local) = route.local_addr()?.ip() else {
        bail!("No se encontró una interfaz IPv4 para el Chromecast")
    };
    let stream = tokio::select! {
        _ = cancel.cancelled() => bail!("Transmisión cancelada"),
        stream = media.cast_stream(playback_url, local, device.address, convert, subtitle) => stream?,
    };
    let initialize = async {
        let mut client =
            CastClient::connect(SocketAddr::from((device.address, device.port))).await?;
        client.launch().await?;
        let mut media = json!({"contentId":stream.url, "contentType":stream.content_type,
            "streamType":if convert { "LIVE" } else { "BUFFERED" }, "metadata":{"metadataType":0,"title":title}});
        if let Some(url) = &stream.subtitle_url {
            media["tracks"] = json!([{"trackId":1,"type":"TEXT","subtype":"SUBTITLES","trackContentId":url,"trackContentType":"text/vtt","name":"Mukure","language":"und"}]);
        }
        let mut load = json!({"type":"LOAD","sessionId":client.session_id,"autoplay":true,"currentTime":if convert {0.0} else {position},"media":media});
        if stream.subtitle_url.is_some() {
            load["activeTrackIds"] = json!([1]);
        }
        let reply = client
            .request(&client.transport.clone(), MEDIA, load)
            .await?;
        client.media_id = reply["status"][0]["mediaSessionId"]
            .as_i64()
            .context("Chromecast no aceptó el video")?;
        Ok::<_, anyhow::Error>((client, reply))
    };
    let (mut client, reply) = tokio::select! {
        _ = cancel.cancelled() => bail!("Transmisión cancelada"),
        _ = stream.cancel.cancelled() => bail!("El video ya no está activo"),
        result = initialize => result?,
    };
    update_status(&status, &reply, &stream.url).await?;
    if let Some(ready) = ready.take() {
        let _ = ready.send(Ok(status.read().await.clone()));
    }
    let mut heartbeat = tokio::time::interval(Duration::from_secs(5));
    let mut poll = tokio::time::interval(Duration::from_secs(1));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_message = tokio::time::Instant::now();
    let session = async {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = stream.cancel.cancelled() => break,
                next = client.next() => {
                    let (namespace, data) = next?;
                    last_message = tokio::time::Instant::now();
                    if namespace == HEARTBEAT && data["type"] == "PING" { client.send("receiver-0", HEARTBEAT, json!({"type":"PONG"})).await?; }
                    else if namespace == MEDIA {
                        if data["status"][0]["media"]["contentId"].as_str().is_some_and(|url| url != stream.url) { client.media_id = 0; }
                        update_status(&status, &data, &stream.url).await?;
                    }
                    else if namespace == RECEIVER {
                        if let Some(volume) = data["status"]["volume"].as_object() {
                            let mut state = status.write().await;
                            state.volume = volume.get("level").and_then(Value::as_f64);
                            state.muted = volume.get("muted").and_then(Value::as_bool).unwrap_or(false);
                        }
                    } else if namespace == CONNECTION && data["type"] == "CLOSE" { break; }
                },
                Some(command) = commands.recv() => {
                    let result = execute(&mut client, &command, !convert).await;
                    if let Ok(data) = &result { update_status(&status, data, &stream.url).await?; }
                    let _ = command.reply.send(result.map(|_| ()));
                    last_message = tokio::time::Instant::now();
                },
                _ = heartbeat.tick() => {
                    ensure!(last_message.elapsed() < Duration::from_secs(20), "Se perdió la conexión con el Chromecast");
                    client.send("receiver-0", HEARTBEAT, json!({"type":"PING"})).await?;
                    client.send_request("receiver-0", RECEIVER, json!({"type":"GET_STATUS"})).await?;
                },
                _ = poll.tick() => { client.send_request(&client.transport.clone(), MEDIA, json!({"type":"GET_STATUS","mediaSessionId":client.media_id})).await?; },
            }
            if !status.read().await.active {
                break;
            }
        }
        Ok(())
    };
    let result = tokio::select! {
        _ = cancel.cancelled() => Ok(()),
        _ = stream.cancel.cancelled() => Ok(()),
        result = session => result,
    };
    stream.cancel.cancel();
    client.stop().await;
    result
}

async fn execute(client: &mut CastClient, command: &Command, can_seek: bool) -> Result<Value> {
    let value = command.value.unwrap_or(0.0);
    match command.action.as_str() {
        "play" => client.media_request(json!({"type":"PLAY"})).await,
        "pause" => client.media_request(json!({"type":"PAUSE"})).await,
        "seek" => {
            ensure!(can_seek, "La conversión para TV no permite saltos");
            client
                .media_request(json!({"type":"SEEK","currentTime":value}))
                .await
        }
        "volume" => {
            client
                .request(
                    "receiver-0",
                    RECEIVER,
                    json!({"type":"SET_VOLUME","volume":{"level":value}}),
                )
                .await
        }
        "mute" => {
            client
                .request(
                    "receiver-0",
                    RECEIVER,
                    json!({"type":"SET_VOLUME","volume":{"muted":value != 0.0}}),
                )
                .await
        }
        _ => bail!("Comando Cast desconocido"),
    }
}
async fn update_status(status: &RwLock<CastStatus>, data: &Value, content_id: &str) -> Result<()> {
    if data["type"] == "LOAD_FAILED" {
        bail!("Chromecast no pudo abrir el video. Probá Convertir para TV");
    }
    if data["type"] != "MEDIA_STATUS" {
        return Ok(());
    }
    let entries = data["status"]
        .as_array()
        .context("Estado de Chromecast inválido")?;
    let mut state = status.write().await;
    let Some(entry) = entries.first() else {
        state.active = false;
        return Ok(());
    };
    if entry["media"]["contentId"]
        .as_str()
        .is_some_and(|url| url != content_id)
    {
        state.active = false;
        return Ok(());
    }
    if let Some(value) = entry["playerState"].as_str() {
        state.player_state = value.into();
    }
    if let Some(value) = entry["currentTime"].as_f64() {
        state.current_time = value.max(0.0);
    }
    if let Some(value) = entry["media"]["duration"].as_f64() {
        state.duration = Some(value);
    }
    if entry["playerState"] == "IDLE" {
        state.active = false;
        if entry["idleReason"] == "ERROR" {
            bail!("El Chromecast encontró un error al reproducir. Probá Convertir para TV");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_lan_addresses_can_be_entered_manually() {
        assert!(lan_address(Ipv4Addr::new(192, 168, 1, 10)));
        assert!(lan_address(Ipv4Addr::new(10, 0, 0, 2)));
        assert!(!lan_address(Ipv4Addr::LOCALHOST));
        assert!(!lan_address(Ipv4Addr::new(8, 8, 8, 8)));
        assert!(!lan_address(Ipv4Addr::UNSPECIFIED));
    }
    #[tokio::test]
    async fn remote_status_tracks_time_and_detects_replaced_content() {
        let state = RwLock::new(CastStatus {
            active: true,
            ..Default::default()
        });
        update_status(&state, &json!({"type":"MEDIA_STATUS","status":[{"mediaSessionId":3,"playerState":"PLAYING","currentTime":12.5,"media":{"contentId":"our-video","duration":100.0}}]}), "our-video").await.unwrap();
        assert_eq!(state.read().await.current_time, 12.5);
        assert_eq!(state.read().await.duration, Some(100.0));
        update_status(
            &state,
            &json!({"type":"MEDIA_STATUS","status":[{"media":{"contentId":"someone-else"}}]}),
            "our-video",
        )
        .await
        .unwrap();
        assert!(!state.read().await.active);
    }
}
