mod media;
#[cfg(test)]
mod torrent_tests;

use anyhow::{anyhow, Context, Result};
use librqbit::{
    api::Api, AddTorrent, AddTorrentOptions, DhtSessionConfig, ManagedTorrent, Session,
    SessionOptions,
};
use media::{MediaServer, MediaSource};
use serde::Serialize;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tauri::{Manager, State};
use tokio::sync::{Mutex, OnceCell};

struct AppState {
    cache_root: PathBuf,
    engine: OnceCell<Arc<Engine>>,
}

struct ActiveTorrent {
    handle: Arc<ManagedTorrent>,
    selected: Option<usize>,
}

struct Engine {
    session: OnceCell<Arc<Session>>,
    media: Arc<MediaServer>,
    active: Mutex<Option<ActiveTorrent>>,
    cache: tempfile::TempDir,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    cache_dir: String,
    ffmpeg_available: bool,
    ffmpeg_path: Option<String>,
    version: &'static str,
}

#[derive(Serialize)]
struct VideoFile {
    id: usize,
    name: String,
    length: u64,
    mime: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TorrentView {
    id: usize,
    name: String,
    files: Vec<VideoFile>,
    selected_file: Option<usize>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TorrentStats {
    state: String,
    progress_bytes: u64,
    total_bytes: u64,
    download_speed: u64,
    upload_speed: u64,
    peers: usize,
    finished: bool,
}

#[derive(Serialize)]
struct Playback {
    url: String,
    name: String,
    mode: String,
    duration: Option<f64>,
}

fn is_video(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|x| x.to_str())
        .map(|x| {
            matches!(
                x.to_ascii_lowercase().as_str(),
                "mp4"
                    | "m4v"
                    | "mov"
                    | "webm"
                    | "mkv"
                    | "avi"
                    | "ts"
                    | "m2ts"
                    | "mpg"
                    | "mpeg"
                    | "ogv"
            )
        })
        .unwrap_or(false)
}

fn mime_for(path: &str) -> String {
    mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string()
}

fn compatible_mode(mode: &str) -> Result<bool> {
    match mode {
        "direct" => Ok(false),
        "compatible" => Ok(true),
        _ => Err(anyhow!("Modo de reproducción desconocido")),
    }
}

impl Engine {
    async fn new(root: &Path) -> Result<Arc<Self>> {
        std::fs::create_dir_all(root).context("No se pudo crear la caché local")?;
        let cache = tempfile::Builder::new()
            .prefix("session-")
            .tempdir_in(root)?;
        let media = MediaServer::start().await?;
        Ok(Arc::new(Self {
            session: OnceCell::new(),
            media,
            active: Mutex::new(None),
            cache,
        }))
    }

    async fn torrent_session(&self) -> Result<&Arc<Session>> {
        // Opening a local video must not bootstrap DHT or contact the network.
        self.session
            .get_or_try_init(|| {
                Session::new_with_opts(
                    self.cache.path().to_path_buf(),
                    SessionOptions {
                        persistence: None,
                        dht: Some(DhtSessionConfig {
                            persistence: None,
                            ..Default::default()
                        }),
                        disable_local_service_discovery: true,
                        ..Default::default()
                    },
                )
            })
            .await
    }

    async fn add(&self, source: AddTorrent<'_>) -> Result<TorrentView> {
        let mut active = self.active.lock().await;
        if active.is_some() {
            return Err(anyhow!(
                "Detén la sesión actual antes de abrir otro torrent"
            ));
        }
        let session = self.torrent_session().await?;
        let response = tokio::time::timeout(
            Duration::from_secs(120),
            session.add_torrent(
                source,
                Some(AddTorrentOptions {
                    paused: true,
                    ..Default::default()
                }),
            ),
        )
        .await
        .context("No llegaron los metadatos en 2 minutos. Comprueba el enlace y sus peers")??;
        let handle = response
            .into_handle()
            .context("El torrent no devolvió metadatos")?;
        let id = handle.id();
        let result = async {
            tokio::time::timeout(Duration::from_secs(120), handle.wait_until_initialized())
                .await
                .context("Se agotó el tiempo al preparar los archivos del torrent")??;
            let api = Api::new(session.clone(), None);
            let details = api.api_torrent_details(id.into())?;
            let files: Vec<_> = details
                .files
                .unwrap_or_default()
                .into_iter()
                .enumerate()
                .filter(|(_, file)| is_video(&file.name))
                .map(|(id, file)| VideoFile {
                    id,
                    mime: mime_for(&file.name),
                    name: file.name,
                    length: file.length,
                })
                .collect();
            if files.is_empty() {
                return Err(anyhow!(
                    "Este torrent no contiene archivos de video reconocidos"
                ));
            }
            Ok(TorrentView {
                id,
                name: details.name.unwrap_or_else(|| "Torrent".into()),
                files,
                selected_file: None,
            })
        }
        .await;
        match result {
            Ok(view) => {
                *active = Some(ActiveTorrent {
                    handle,
                    selected: None,
                });
                Ok(view)
            }
            Err(error) => {
                let _ = session.delete(id.into(), true).await;
                Err(error)
            }
        }
    }

    async fn shutdown(&self) {
        self.media.shutdown().await;
        if let Some(session) = self.session.get() {
            session.stop().await;
        }
        // Tauri can terminate before Rust destructors run, so clean up explicitly.
        let _ = tokio::fs::remove_dir_all(self.cache.path()).await;
    }
}

async fn engine(state: &AppState) -> Result<&Arc<Engine>, String> {
    state
        .engine
        .get_or_try_init(|| Engine::new(&state.cache_root))
        .await
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
async fn get_app_info(state: State<'_, AppState>) -> Result<AppInfo, String> {
    let e = engine(&state).await?;
    Ok(AppInfo {
        cache_dir: e.cache.path().to_string_lossy().into_owned(),
        ffmpeg_available: e.media.ffmpeg_path().is_some(),
        ffmpeg_path: e
            .media
            .ffmpeg_path()
            .map(|p| p.to_string_lossy().into_owned()),
        version: env!("CARGO_PKG_VERSION"),
    })
}

#[tauri::command]
async fn add_magnet(magnet: String, state: State<'_, AppState>) -> Result<TorrentView, String> {
    let magnet = magnet.trim();
    if !magnet.starts_with("magnet:?") || magnet.len() > 16_384 || !magnet.contains("xt=urn:btih:")
    {
        return Err("Introduce un magnet BitTorrent v1 válido (magnet:?xt=urn:btih:...)".into());
    }
    engine(&state)
        .await?
        .add(AddTorrent::from_url(magnet))
        .await
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
async fn add_torrent_file(path: String, state: State<'_, AppState>) -> Result<TorrentView, String> {
    let path = PathBuf::from(path);
    if path.extension().and_then(|s| s.to_str()) != Some("torrent") {
        return Err("Selecciona un archivo .torrent".into());
    }
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|e| e.to_string())?;
    if metadata.len() > 16 * 1024 * 1024 {
        return Err("El archivo .torrent supera los 16 MiB".into());
    }
    let bytes = tokio::fs::read(path).await.map_err(|e| e.to_string())?;
    engine(&state)
        .await?
        .add(AddTorrent::from_bytes(bytes))
        .await
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
async fn select_file(
    id: usize,
    file_id: usize,
    mode: String,
    state: State<'_, AppState>,
) -> Result<Playback, String> {
    let compatible = compatible_mode(&mode).map_err(|e| e.to_string())?;
    let e = engine(&state).await?;
    let mut active = e.active.lock().await;
    let torrent = active
        .as_mut()
        .filter(|t| t.handle.id() == id)
        .ok_or("La sesión ya no está activa")?;
    let session = e.session.get().ok_or("La sesión ya no está activa")?;
    let details = Api::new(session.clone(), None)
        .api_torrent_details(id.into())
        .map_err(|e| e.to_string())?;
    let files = details.files.ok_or("No hay archivos disponibles")?;
    let file = files
        .get(file_id)
        .filter(|f| is_video(&f.name))
        .ok_or("Selecciona un archivo de video válido")?;
    if compatible && e.media.ffmpeg_path().is_none() {
        return Err("FFmpeg no está instalado. Usa el modo directo o instala FFmpeg y vuelve a abrir la app".into());
    }
    e.media.clear().await;
    session
        .update_only_files(&torrent.handle, &HashSet::from([file_id]))
        .await
        .map_err(|e| e.to_string())?;
    // Unpause is necessary only before the first selection; repeating it on a live torrent is an error.
    if torrent.selected.is_none() {
        session
            .unpause(&torrent.handle)
            .await
            .map_err(|e| e.to_string())?;
    }
    let url = e
        .media
        .register(
            MediaSource::Torrent {
                handle: torrent.handle.clone(),
                file_id,
            },
            mime_for(&file.name),
            compatible,
        )
        .await
        .map_err(|e| format!("{e:#}"))?;
    torrent.selected = Some(file_id);
    Ok(Playback {
        url,
        name: file.name.clone(),
        mode,
        duration: None,
    })
}

#[tauri::command]
async fn get_torrent_stats(id: usize, state: State<'_, AppState>) -> Result<TorrentStats, String> {
    let e = engine(&state).await?;
    let active = e.active.lock().await;
    let torrent = active
        .as_ref()
        .filter(|t| t.handle.id() == id)
        .ok_or("La sesión ya no está activa")?;
    let stats = torrent.handle.stats();
    if let Some(error) = stats.error {
        return Err(error);
    }
    let (download_speed, upload_speed, peers) = stats
        .live
        .as_ref()
        .map(|live| {
            let peers = live.snapshot.peer_stats.live as usize;
            (
                live.download_speed.as_bytes(),
                live.upload_speed.as_bytes(),
                peers,
            )
        })
        .unwrap_or((0, 0, 0));
    Ok(TorrentStats {
        state: stats.state.to_string(),
        progress_bytes: stats.progress_bytes,
        total_bytes: stats.total_bytes,
        download_speed,
        upload_speed,
        peers,
        finished: stats.finished,
    })
}

#[tauri::command]
async fn remove_torrent(id: usize, state: State<'_, AppState>) -> Result<(), String> {
    let e = engine(&state).await?;
    let mut active = e.active.lock().await;
    let torrent = active
        .as_ref()
        .filter(|t| t.handle.id() == id)
        .ok_or("La sesión ya no está activa")?;
    e.media.clear().await;
    e.session
        .get()
        .ok_or("La sesión ya no está activa")?
        .delete(torrent.handle.id().into(), true)
        .await
        .map_err(|e| e.to_string())?;
    *active = None;
    Ok(())
}

#[tauri::command]
async fn open_local_video(
    path: String,
    mode: String,
    state: State<'_, AppState>,
) -> Result<Playback, String> {
    let compatible = compatible_mode(&mode).map_err(|e| e.to_string())?;
    let path = tokio::fs::canonicalize(path)
        .await
        .map_err(|e| e.to_string())?;
    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|e| e.to_string())?;
    if !meta.is_file() || !is_video(&path.to_string_lossy()) {
        return Err("Selecciona un archivo de video".into());
    }
    let e = engine(&state).await?;
    if e.active.lock().await.is_some() {
        return Err("Detén la sesión torrent antes de abrir un video local".into());
    }
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let url = e
        .media
        .register(
            MediaSource::Local(path.clone()),
            mime_for(&path.to_string_lossy()),
            compatible,
        )
        .await
        .map_err(|e| format!("{e:#}"))?;
    Ok(Playback {
        url,
        name,
        mode,
        duration: None,
    })
}

#[tauri::command]
async fn stop_playback(state: State<'_, AppState>) -> Result<(), String> {
    if let Some(e) = state.engine.get() {
        e.media.clear().await;
    }
    Ok(())
}

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            app.manage(AppState {
                cache_root: app.path().app_cache_dir()?.join("sessions"),
                engine: OnceCell::new(),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_app_info,
            add_magnet,
            add_torrent_file,
            select_file,
            get_torrent_stats,
            remove_torrent,
            open_local_video,
            stop_playback
        ])
        .build(tauri::generate_context!())
        .expect("No se pudo iniciar Mukure");
    app.run(|app, event| {
        // macOS's native Quit menu can emit Exit directly, without an
        // ExitRequested round trip. Finish cleanup before returning to Cocoa.
        if let tauri::RunEvent::Exit = event {
            let state = app.state::<AppState>();
            if let Some(e) = state.engine.get() {
                tauri::async_runtime::block_on(e.shutdown());
            }
        }
    });
}

#[cfg(test)]
mod lifecycle_tests {
    use super::Engine;

    #[tokio::test]
    async fn local_playback_initialization_stays_offline_and_cleanup_removes_cache() {
        let root = tempfile::tempdir().unwrap();
        let engine = Engine::new(root.path()).await.unwrap();
        let path = engine.cache.path().to_owned();
        assert!(path.is_dir());
        assert!(engine.session.get().is_none());
        tokio::fs::write(path.join("download.tmp"), b"temporary data")
            .await
            .unwrap();
        engine.shutdown().await;
        assert!(!path.exists());
    }
}
