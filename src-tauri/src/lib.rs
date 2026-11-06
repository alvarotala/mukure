mod cast_protocol;
mod casting;
mod embedded;
mod media;
mod online_subtitles;
mod provider_settings;
#[cfg(test)]
mod subtitle_torrent_tests;
mod subtitles;
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
use subtitles::{SubtitleTrack, MAX_SUBTITLE_BYTES};
use tauri::{Manager, State};
use tokio::io::AsyncReadExt;
use tokio::sync::{Mutex, OnceCell};
use tokio_util::sync::CancellationToken;

struct AppState {
    cache_root: PathBuf,
    engine: OnceCell<Arc<Engine>>,
}

struct ActiveTorrent {
    handle: Arc<ManagedTorrent>,
    selected: Option<usize>,
    subtitles_cancel: CancellationToken,
}

struct EmbeddedRequest {
    playback_url: String,
    request_id: String,
    cancel: CancellationToken,
}

struct Engine {
    cast: Arc<casting::CastManager>,
    session: OnceCell<Arc<Session>>,
    media: Arc<MediaServer>,
    active: Mutex<Option<ActiveTorrent>>,
    cache: tempfile::TempDir,
    embedded_request: Mutex<Option<EmbeddedRequest>>,
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
    subtitles: Vec<VideoFile>,
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
            cast: Arc::new(casting::CastManager::default()),
            session: OnceCell::new(),
            media,
            active: Mutex::new(None),
            cache,
            embedded_request: Mutex::new(None),
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
            let mut files = Vec::new();
            let mut subtitles = Vec::new();
            for (id, file) in details.files.unwrap_or_default().into_iter().enumerate() {
                let video = is_video(&file.name);
                let subtitle = subtitles::is_subtitle(&file.name);
                if !video && !subtitle {
                    continue;
                }
                let item = VideoFile {
                    id,
                    mime: mime_for(&file.name),
                    name: file.name,
                    length: file.length,
                };
                if video {
                    files.push(item);
                } else {
                    subtitles.push(item);
                }
            }
            if files.is_empty() {
                return Err(anyhow!(
                    "Este torrent no contiene archivos de video reconocidos"
                ));
            }
            Ok(TorrentView {
                id,
                name: details.name.unwrap_or_else(|| "Torrent".into()),
                files,
                subtitles,
                selected_file: None,
            })
        }
        .await;
        match result {
            Ok(view) => {
                *active = Some(ActiveTorrent {
                    handle,
                    selected: None,
                    subtitles_cancel: CancellationToken::new(),
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
        self.cast.stop().await;
        self.media.shutdown().await;
        if let Some(session) = self.session.get() {
            session.stop().await;
        }
        // Tauri can terminate before Rust destructors run, so clean up explicitly.
        let _ = tokio::fs::remove_dir_all(self.cache.path()).await;
    }

    async fn read_torrent_subtitle(&self, id: usize, file_id: usize) -> Result<SubtitleTrack> {
        // Snapshot the handle, then release the lock so playback, stats and
        // closing the torrent remain responsive while pieces arrive.
        let (handle, cancel, name, length) = {
            let active = self.active.lock().await;
            let torrent = active
                .as_ref()
                .filter(|t| t.handle.id() == id)
                .context("La sesión ya no está activa")?;
            anyhow::ensure!(torrent.selected.is_some(), "Primero selecciona un video");
            let session = self.session.get().context("La sesión ya no está activa")?;
            let details = Api::new(session.clone(), None).api_torrent_details(id.into())?;
            let files = details.files.context("No hay archivos disponibles")?;
            let file = files
                .get(file_id)
                .filter(|f| subtitles::is_subtitle(&f.name))
                .context("Selecciona un subtítulo .srt o .vtt")?;
            anyhow::ensure!(
                file.length <= MAX_SUBTITLE_BYTES,
                "El subtítulo supera los 5 MiB"
            );
            (
                torrent.handle.clone(),
                torrent.subtitles_cancel.clone(),
                file.name.clone(),
                file.length,
            )
        };
        tokio::select! {
            _ = cancel.cancelled() => Err(anyhow!("La selección de video cambió")),
            result = tokio::time::timeout(Duration::from_secs(45), async {
                let stream = handle.stream(file_id).await?;
                let mut bytes = Vec::new();
                stream.take(MAX_SUBTITLE_BYTES + 1).read_to_end(&mut bytes).await?;
                anyhow::ensure!(bytes.len() as u64 == length, "El subtítulo está incompleto");
                subtitles::parse(name, &bytes)
            }) => result.context("No llegaron los subtítulos en 45 segundos. Comprueba los peers y vuelve a intentar")?,
        }
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
    e.cast.stop().await;
    e.media.clear().await;
    if torrent.selected != Some(file_id) {
        torrent.subtitles_cancel.cancel();
        torrent.subtitles_cancel = CancellationToken::new();
    }
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
    e.cast.stop().await;
    e.media.clear().await;
    torrent.subtitles_cancel.cancel();
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
    e.cast.stop().await;
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
        e.cast.stop().await;
        e.media.clear().await;
    }
    Ok(())
}

#[tauri::command]
async fn read_subtitle_file(path: String) -> Result<SubtitleTrack, String> {
    subtitles::read_local(PathBuf::from(path))
        .await
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
async fn search_online_subtitles(
    playback_url: String,
    provider: String,
    query: String,
    language: String,
    state: State<'_, AppState>,
) -> Result<online_subtitles::SearchResult, String> {
    let e = engine(&state).await?;
    let (_, cancel) = e
        .media
        .subtitle_source(&playback_url)
        .await
        .map_err(|e| e.to_string())?;
    let provider_id = provider.clone();
    let key = if matches!(provider.as_str(), "subdl" | "wyzie") {
        tauri::async_runtime::spawn_blocking(move || provider_settings::key(&provider_id))
            .await
            .map_err(|_| "No se pudo consultar el Llavero")?
            .map_err(|e| e.to_string())?
    } else {
        None
    };
    tokio::select! {
        _ = cancel.cancelled() => Err("El video ya no está activo".into()),
        result = online_subtitles::search(&provider, &query, &language, key.as_deref()) => result.map_err(|e| format!("{e:#}")),
    }
}

#[tauri::command]
async fn download_online_subtitle(
    playback_url: String,
    url: String,
    name: String,
    state: State<'_, AppState>,
) -> Result<SubtitleTrack, String> {
    let e = engine(&state).await?;
    let (_, cancel) = e
        .media
        .subtitle_source(&playback_url)
        .await
        .map_err(|e| e.to_string())?;
    tokio::select! {
        _ = cancel.cancelled() => Err("El video ya no está activo".into()),
        result = online_subtitles::download(&url, &name) => result.map_err(|e| format!("{e:#}")),
    }
}

#[derive(Serialize)]
struct LocalSubtitle {
    name: String,
    path: String,
}

#[tauri::command]
async fn find_local_subtitles(path: String) -> Result<Vec<LocalSubtitle>, String> {
    let video = PathBuf::from(path);
    let stem = video
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("Nombre de video inválido")?
        .to_lowercase();
    let parent = video.parent().ok_or("Carpeta de video inválida")?;
    let mut directory = tokio::fs::read_dir(parent)
        .await
        .map_err(|e| e.to_string())?;
    let mut results = Vec::new();
    while let Some(entry) = directory.next_entry().await.map_err(|e| e.to_string())? {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let candidate = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        if subtitles::is_subtitle(&name)
            && (candidate == stem || candidate.starts_with(&format!("{stem}.")))
            && entry
                .file_type()
                .await
                .map_err(|e| e.to_string())?
                .is_file()
        {
            results.push(LocalSubtitle {
                name,
                path: path.to_string_lossy().into_owned(),
            });
        }
        if results.len() == 100 {
            break;
        }
    }
    results.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(results)
}

#[tauri::command]
async fn read_torrent_subtitle(
    id: usize,
    file_id: usize,
    state: State<'_, AppState>,
) -> Result<SubtitleTrack, String> {
    engine(&state)
        .await?
        .read_torrent_subtitle(id, file_id)
        .await
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
async fn get_embedded_subtitles(
    playback_url: String,
    state: State<'_, AppState>,
) -> Result<Vec<embedded::EmbeddedTrack>, String> {
    let e = engine(&state).await?;
    let (input, cancel) = e
        .media
        .subtitle_source(&playback_url)
        .await
        .map_err(|e| e.to_string())?;
    let probe = e.media.ffprobe_path().ok_or(
        "FFprobe no está disponible. Reinicia con FFprobe instalado para buscar pistas integradas",
    )?;
    embedded::probe(probe, &input, cancel)
        .await
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
async fn start_embedded_subtitle(
    playback_url: String,
    track_id: usize,
    request_id: String,
    start_time: f64,
    on_event: tauri::ipc::Channel<embedded::SubtitleEvent>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let e = engine(&state).await?;
    let (input, source_cancel) = e
        .media
        .subtitle_source(&playback_url)
        .await
        .map_err(|e| e.to_string())?;
    let ffmpeg = e
        .media
        .ffmpeg_path()
        .ok_or("FFmpeg no está disponible")?
        .to_path_buf();
    let cancel = source_cancel.child_token();
    let mut active = e.embedded_request.lock().await;
    if source_cancel.is_cancelled() {
        return Err("El video ya no está activo".into());
    }
    if let Some(previous) = active.take() {
        previous.cancel.cancel();
    }
    *active = Some(EmbeddedRequest {
        playback_url,
        request_id,
        cancel: cancel.clone(),
    });
    tauri::async_runtime::spawn(async move {
        if let Err(error) = embedded::extract(
            &ffmpeg,
            &input,
            track_id,
            start_time,
            cancel.clone(),
            on_event.clone(),
        )
        .await
        {
            if !cancel.is_cancelled() {
                let _ = on_event.send(embedded::SubtitleEvent::Error {
                    message: format!("{error:#}"),
                });
            }
        }
    });
    Ok(())
}

#[tauri::command]
async fn stop_embedded_subtitle(
    playback_url: String,
    request_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if let Some(e) = state.engine.get() {
        let mut active = e.embedded_request.lock().await;
        if active
            .as_ref()
            .is_some_and(|r| r.playback_url == playback_url && r.request_id == request_id)
        {
            if let Some(request) = active.take() {
                request.cancel.cancel();
            }
        }
    }
    Ok(())
}

#[tauri::command]
async fn discover_cast_devices(
    state: State<'_, AppState>,
) -> Result<Vec<casting::CastDevice>, String> {
    engine(&state)
        .await?
        .cast
        .discover()
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
async fn get_cast_status(state: State<'_, AppState>) -> Result<casting::CastStatus, String> {
    Ok(engine(&state).await?.cast.status().await)
}
#[tauri::command]
async fn start_cast(
    playback_url: String,
    device_id: Option<String>,
    address: Option<String>,
    title: String,
    position: f64,
    convert: bool,
    subtitle: Option<String>,
    state: State<'_, AppState>,
) -> Result<casting::CastStatus, String> {
    let e = engine(&state).await?;
    e.cast
        .start(
            e.media.clone(),
            playback_url,
            device_id,
            address,
            title,
            position,
            convert,
            subtitle,
        )
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
async fn cast_action(
    action: String,
    value: Option<f64>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    engine(&state)
        .await?
        .cast
        .action(action, value)
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
async fn stop_cast(state: State<'_, AppState>) -> Result<(), String> {
    engine(&state).await?.cast.stop().await;
    Ok(())
}

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            app.manage(AppState {
                cache_root: app.path().app_cache_dir()?.join("sessions"),
                engine: OnceCell::new(),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_app_info,
            discover_cast_devices,
            get_cast_status,
            start_cast,
            cast_action,
            stop_cast,
            add_magnet,
            add_torrent_file,
            select_file,
            get_torrent_stats,
            remove_torrent,
            open_local_video,
            stop_playback,
            read_subtitle_file,
            find_local_subtitles,
            search_online_subtitles,
            download_online_subtitle,
            provider_settings::get_subtitle_providers,
            provider_settings::connect_subtitle_provider,
            provider_settings::disconnect_subtitle_provider,
            provider_settings::open_subtitle_provider_page,
            read_torrent_subtitle,
            get_embedded_subtitles,
            start_embedded_subtitle,
            stop_embedded_subtitle
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
