//! Private, process-local media transport for the system WebView.
//!
//! Torrent readers prioritize the pieces around each requested byte range. The
//! compatibility stream is deliberately non-seekable: FFmpeg writes one fMP4
//! stream from the beginning, and is terminated when that response is dropped.

use std::{
    io::{self, SeekFrom},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use anyhow::{bail, Context, Result};
use axum::{
    body::{Body, Bytes},
    extract::{Path as RoutePath, State},
    http::{header, HeaderMap, HeaderValue, Method, Request, Response, StatusCode},
    middleware::{self, Next},
    routing::get,
    Router,
};
use futures_util::StreamExt;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt},
    net::TcpListener,
    process::Command,
    sync::{mpsc, Mutex, RwLock},
    task::JoinHandle,
};
use tokio_util::{io::ReaderStream, sync::CancellationToken};
use uuid::Uuid;

#[derive(Clone)]
pub enum MediaSource {
    Local(PathBuf),
    Torrent {
        handle: Arc<librqbit::ManagedTorrent>,
        file_id: usize,
    },
}

struct MediaEntry {
    id: String,
    playback_url: String,
    source: MediaSource,
    mime: HeaderValue,
    cancel: CancellationToken,
    transcode: Mutex<Option<CancellationToken>>,
}

struct Shared {
    token: String,
    origin: String,
    active: RwLock<Option<Arc<MediaEntry>>>,
    ffmpeg: Option<PathBuf>,
    ffprobe: Option<PathBuf>,
    // A second request cancels its predecessor before acquiring this slot.
    // This also prevents two processes briefly overlapping when sources change.
    ffmpeg_slot: Arc<Mutex<()>>,
    shutdown: CancellationToken,
}

pub struct MediaServer {
    shared: Arc<Shared>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl MediaServer {
    pub async fn start() -> Result<Arc<Self>> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .context("No se pudo abrir el transporte local de video")?;
        let address = listener.local_addr()?;
        let shared = Arc::new(Shared {
            token: Uuid::new_v4().simple().to_string(),
            origin: format!("http://127.0.0.1:{}", address.port()),
            active: RwLock::new(None),
            ffmpeg: find_ffmpeg().await,
            ffprobe: find_media_tool("ffprobe", "MUKURE_FFPROBE").await,
            ffmpeg_slot: Arc::new(Mutex::new(())),
            shutdown: CancellationToken::new(),
        });
        let router = Router::new()
            .route("/{token}/stream/{id}", get(direct).head(direct))
            .route("/{token}/compatible/{id}", get(compatible).head(compatible))
            .layer(middleware::from_fn(local_webview_cors))
            .with_state(shared.clone());
        let shutdown = shared.shutdown.clone();
        let task = tokio::spawn(async move {
            // No request logging: media URLs contain a per-process secret.
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await;
        });
        Ok(Arc::new(Self {
            shared,
            task: Mutex::new(Some(task)),
        }))
    }

    pub fn ffmpeg_path(&self) -> Option<&Path> {
        self.shared.ffmpeg.as_deref()
    }

    pub fn ffprobe_path(&self) -> Option<&Path> {
        self.shared.ffprobe.as_deref()
    }

    pub async fn subtitle_source(&self, playback_url: &str) -> Result<(String, CancellationToken)> {
        let active = self.shared.active.read().await;
        let entry = active
            .as_ref()
            .filter(|entry| entry.playback_url == playback_url && !entry.cancel.is_cancelled())
            .context("El video ya no está activo")?;
        Ok((
            format!(
                "{}/{}/stream/{}",
                self.shared.origin, self.shared.token, entry.id
            ),
            entry.cancel.child_token(),
        ))
    }

    pub async fn register(
        &self,
        source: MediaSource,
        mime: String,
        compatible: bool,
    ) -> Result<String> {
        if compatible && self.shared.ffmpeg.is_none() {
            bail!("FFmpeg no está disponible. Instálalo o configura MUKURE_FFMPEG y vuelve a abrir la app.");
        }
        let mime = HeaderValue::from_str(&mime).context("Tipo de video inválido")?;
        self.clear().await;
        let id = Uuid::new_v4().simple().to_string();
        let route = if compatible { "compatible" } else { "stream" };
        let url = format!(
            "{}/{}/{}/{}",
            self.shared.origin, self.shared.token, route, id
        );
        *self.shared.active.write().await = Some(Arc::new(MediaEntry {
            id,
            playback_url: url.clone(),
            source,
            mime,
            cancel: self.shared.shutdown.child_token(),
            transcode: Mutex::new(None),
        }));
        Ok(url)
    }

    pub async fn clear(&self) {
        if let Some(entry) = self.shared.active.write().await.take() {
            entry.cancel.cancel();
        }
    }

    pub async fn shutdown(&self) {
        self.clear().await;
        self.shared.shutdown.cancel();
        if let Some(mut task) = self.task.lock().await.take() {
            if tokio::time::timeout(Duration::from_secs(3), &mut task)
                .await
                .is_err()
            {
                task.abort();
                let _ = task.await;
            }
        }
    }
}

impl Drop for MediaServer {
    fn drop(&mut self) {
        self.shared.shutdown.cancel();
    }
}

trait ReadSeek: AsyncRead + AsyncSeek + Unpin + Send {}
impl<T: AsyncRead + AsyncSeek + Unpin + Send> ReadSeek for T {}

async fn open_source(source: &MediaSource) -> Result<(Box<dyn ReadSeek>, u64)> {
    match source {
        MediaSource::Local(path) => {
            let file = tokio::fs::File::open(path).await?;
            let length = file.metadata().await?.len();
            Ok((Box::new(file), length))
        }
        MediaSource::Torrent { handle, file_id } => {
            let stream = handle.clone().stream(*file_id).await?;
            let length = stream.len();
            Ok((Box::new(stream), length))
        }
    }
}

async fn get_entry(state: &Shared, token: &str, id: &str) -> Option<Arc<MediaEntry>> {
    if token != state.token || state.shutdown.is_cancelled() {
        return None;
    }
    state
        .active
        .read()
        .await
        .as_ref()
        .filter(|entry| entry.id == id && !entry.cancel.is_cancelled())
        .cloned()
}

fn empty_response(status: StatusCode) -> Response<Body> {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

async fn local_webview_cors(request: Request<Body>, next: Next) -> Response<Body> {
    // MediaSource needs fetch() rather than a media-element navigation. Allow
    // only the app's WebView origins and the exact local development origin;
    // the per-process token remains mandatory on every media request.
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .filter(|origin| {
            matches!(
                origin.to_str(),
                Ok("tauri://localhost")
                    | Ok("http://tauri.localhost")
                    | Ok("https://tauri.localhost")
                    | Ok("http://127.0.0.1:1420")
            )
        })
        .cloned();
    let mut response = next.run(request).await;
    if let Some(origin) = origin {
        response
            .headers_mut()
            .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        response
            .headers_mut()
            .append(header::VARY, HeaderValue::from_static("Origin"));
    }
    response
}

async fn direct(
    State(state): State<Arc<Shared>>,
    RoutePath((token, id)): RoutePath<(String, String)>,
    method: Method,
    headers: HeaderMap,
) -> Response<Body> {
    let Some(entry) = get_entry(&state, &token, &id).await else {
        return empty_response(StatusCode::NOT_FOUND);
    };
    direct_entry(entry, method, headers).await
}

async fn direct_entry(
    entry: Arc<MediaEntry>,
    method: Method,
    headers: HeaderMap,
) -> Response<Body> {
    let opened = tokio::select! {
        _ = entry.cancel.cancelled() => return empty_response(StatusCode::GONE),
        value = open_source(&entry.source) => value,
    };
    let (mut reader, total) = match opened {
        Ok(value) => value,
        Err(_) => return empty_response(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let range_headers = headers.get_all(header::RANGE);
    let mut range_headers = range_headers.iter();
    let range = range_headers.next();
    let resolved = if range_headers.next().is_some() {
        Err(())
    } else {
        match range {
            Some(value) => value
                .to_str()
                .map_err(|_| ())
                .and_then(|value| resolve_range(Some(value), total)),
            None => resolve_range(None, total),
        }
    };
    let resolved = match resolved {
        Ok(range) => range,
        Err(()) => {
            let mut response = empty_response(StatusCode::RANGE_NOT_SATISFIABLE);
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{total}")).unwrap(),
            );
            response
                .headers_mut()
                .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            return response;
        }
    };
    let mut response = empty_response(if resolved.partial {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    });
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, entry.mime.clone());
    response
        .headers_mut()
        .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&resolved.length.to_string()).unwrap(),
    );
    if resolved.partial {
        response.headers_mut().insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!(
                "bytes {}-{}/{total}",
                resolved.start,
                resolved.start + resolved.length - 1,
            ))
            .unwrap(),
        );
    }
    // HEAD reports the same range without waiting for any downloaded piece.
    if method == Method::HEAD || resolved.length == 0 {
        return response;
    }
    let seek = tokio::select! {
        _ = entry.cancel.cancelled() => return empty_response(StatusCode::GONE),
        result = reader.seek(SeekFrom::Start(resolved.start)) => result,
    };
    if seek.is_err() {
        return empty_response(StatusCode::INTERNAL_SERVER_ERROR);
    }
    let mut chunks = ReaderStream::with_capacity(reader.take(resolved.length), 128 * 1024);
    let stream = async_stream::stream! {
        loop {
            tokio::select! {
                _ = entry.cancel.cancelled() => break,
                next = chunks.next() => match next {
                    Some(chunk) => yield chunk,
                    None => break,
                }
            }
        }
    };
    *response.body_mut() = Body::from_stream(stream);
    response
}

async fn compatible(
    State(state): State<Arc<Shared>>,
    RoutePath((token, id)): RoutePath<(String, String)>,
    method: Method,
) -> Response<Body> {
    let Some(entry) = get_entry(&state, &token, &id).await else {
        return empty_response(StatusCode::NOT_FOUND);
    };
    let Some(ffmpeg) = state.ffmpeg.clone() else {
        return empty_response(StatusCode::SERVICE_UNAVAILABLE);
    };
    let mut response = empty_response(StatusCode::OK);
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("video/mp4"));
    response
        .headers_mut()
        .insert(header::ACCEPT_RANGES, HeaderValue::from_static("none"));
    if method == Method::HEAD {
        return response;
    }

    let cancel = entry.cancel.child_token();
    if let Some(previous) = entry.transcode.lock().await.replace(cancel.clone()) {
        previous.cancel();
    }
    let input = format!("{}/{}/stream/{}", state.origin, state.token, entry.id);
    let (sender, mut receiver) = mpsc::channel::<io::Result<Bytes>>(4);
    let worker_cancel = cancel.clone();
    let slot = state.ffmpeg_slot.clone();
    tokio::spawn(async move {
        let _permit = tokio::select! {
            _ = worker_cancel.cancelled() => return,
            permit = slot.lock_owned() => permit,
        };
        let result = transcode(&ffmpeg, &input, &worker_cancel, &sender).await;
        if result.is_err() && !worker_cancel.is_cancelled() {
            // Do not forward FFmpeg's stderr: it can contain the input secret.
            let _ = sender
                .send(Err(io::Error::other("FFmpeg no pudo convertir este video")))
                .await;
        }
    });
    let on_drop = CancelOnDrop(cancel);
    let stream = async_stream::stream! {
        // Hyper may drop a paused response without polling it again. A guard
        // cancels the independent worker even in that case.
        let _on_drop = on_drop;
        while let Some(chunk) = receiver.recv().await {
            yield chunk;
        }
    };
    *response.body_mut() = Body::from_stream(stream);
    response
}

struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

async fn transcode(
    ffmpeg: &Path,
    input: &str,
    cancel: &CancellationToken,
    output: &mpsc::Sender<io::Result<Bytes>>,
) -> Result<()> {
    let mut child = Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-rw_timeout",
            "45000000",
            "-i",
            input,
            "-map",
            "0:v:0",
            "-map",
            "0:a:0?",
            "-sn",
            "-dn",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-crf",
            "23",
            "-pix_fmt",
            "yuv420p",
            "-vf",
            "scale=trunc(iw/2)*2:trunc(ih/2)*2",
            "-g",
            "48",
            "-c:a",
            "aac",
            "-b:a",
            "160k",
            "-ac",
            "2",
            "-movflags",
            "frag_keyframe+empty_moov+default_base_moof",
            "-f",
            "mp4",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("No se pudo iniciar FFmpeg")?;
    let stdout = child.stdout.take().context("FFmpeg sin salida de video")?;
    let mut chunks = ReaderStream::with_capacity(stdout, 128 * 1024);
    loop {
        let next = tokio::select! {
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Ok(());
            },
            next = chunks.next() => next,
        };
        let Some(chunk) = next else { break };
        tokio::select! {
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Ok(());
            },
            sent = output.send(chunk) => if sent.is_err() {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Ok(());
            },
        }
    }
    let status = tokio::select! {
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Ok(());
        },
        status = child.wait() => status?,
    };
    if !status.success() {
        bail!("FFmpeg terminó sin completar el video");
    }
    Ok(())
}

async fn find_ffmpeg() -> Option<PathBuf> {
    find_media_tool("ffmpeg", "MUKURE_FFMPEG").await
}

async fn find_media_tool(tool: &str, variable: &str) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os(variable) {
        candidates.push(PathBuf::from(path));
    }
    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            // Tauri macOS bundles place executables in Contents/MacOS and
            // bundled resources in Contents/Resources. This works from Finder,
            // where Homebrew and developer-tool paths may be absent from PATH.
            if let Some(contents) = directory.parent() {
                candidates.push(contents.join(format!("Resources/bin/{tool}")));
            }
            candidates.push(directory.join(format!("bin/{tool}")));
            candidates.push(directory.join(format!("resources/bin/{tool}")));
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&paths).map(|path| {
            path.join(if cfg!(windows) {
                format!("{tool}.exe")
            } else {
                tool.to_owned()
            })
        }));
    }
    candidates.extend([
        PathBuf::from(format!("/opt/homebrew/bin/{tool}")),
        PathBuf::from(format!("/usr/local/bin/{tool}")),
    ]);
    if let Ok(directory) = std::env::current_dir() {
        for root in [Some(directory.as_path()), directory.parent()]
            .into_iter()
            .flatten()
        {
            candidates.push(root.join(format!(".tools/bin/{tool}")));
            candidates.push(root.join(format!(".tools/{tool}")));
            candidates.push(root.join(format!("node_modules/{tool}-static/{tool}")));
        }
    }
    for path in candidates {
        if !path.is_file() {
            continue;
        }
        let status = tokio::time::timeout(
            Duration::from_secs(5),
            Command::new(&path)
                .arg("-version")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .status(),
        )
        .await;
        if matches!(status, Ok(Ok(status)) if status.success()) {
            return Some(path);
        }
    }
    None
}

#[derive(Debug, PartialEq, Eq)]
struct ResolvedRange {
    start: u64,
    length: u64,
    partial: bool,
}

fn resolve_range(header: Option<&str>, total: u64) -> Result<ResolvedRange, ()> {
    let Some(header) = header else {
        return Ok(ResolvedRange {
            start: 0,
            length: total,
            partial: false,
        });
    };
    let spec = header.trim().strip_prefix("bytes=").ok_or(())?;
    if spec.contains(',') || total == 0 {
        return Err(());
    }
    let (start, end) = spec.split_once('-').ok_or(())?;
    let number = |value: &str| -> Result<u64, ()> {
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(());
        }
        value.parse().map_err(|_| ())
    };
    let (start, end) = if start.is_empty() {
        let length = number(end)?;
        if length == 0 {
            return Err(());
        }
        (total.saturating_sub(length), total - 1)
    } else {
        let start = number(start)?;
        if start >= total {
            return Err(());
        }
        let end = if end.is_empty() {
            total - 1
        } else {
            number(end)?.min(total - 1)
        };
        if end < start {
            return Err(());
        }
        (start, end)
    };
    Ok(ResolvedRange {
        start,
        length: end - start + 1,
        partial: true,
    })
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn subtitle_input_is_bound_to_current_playback_and_revoked_on_clear() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let server = super::MediaServer::start().await.unwrap();
        let url = server
            .register(
                super::MediaSource::Local(file.path().to_owned()),
                "video/mp4".into(),
                false,
            )
            .await
            .unwrap();
        assert!(server
            .subtitle_source("http://example.com/video")
            .await
            .is_err());
        let (input, cancel) = server.subtitle_source(&url).await.unwrap();
        assert_eq!(input, url);
        assert!(!cancel.is_cancelled());
        server.clear().await;
        assert!(cancel.is_cancelled());
        assert!(server.subtitle_source(&url).await.is_err());
        server.shutdown().await;
    }
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn full_and_empty_files() {
        assert_eq!(
            resolve_range(None, 100),
            Ok(ResolvedRange {
                start: 0,
                length: 100,
                partial: false
            })
        );
        assert_eq!(
            resolve_range(None, 0),
            Ok(ResolvedRange {
                start: 0,
                length: 0,
                partial: false
            })
        );
        assert!(resolve_range(Some("bytes=0-0"), 0).is_err());
    }

    #[test]
    fn bounded_open_and_suffix_ranges() {
        for (header, start, length) in [
            ("bytes=0-0", 0, 1),
            ("bytes=20-39", 20, 20),
            ("bytes=20-", 20, 80),
            ("bytes=99-200", 99, 1),
            ("bytes=-15", 85, 15),
            ("bytes=-200", 0, 100),
        ] {
            assert_eq!(
                resolve_range(Some(header), 100),
                Ok(ResolvedRange {
                    start,
                    length,
                    partial: true
                })
            );
        }
    }

    #[test]
    fn rejects_invalid_unsatisfiable_and_multiple_ranges() {
        for header in [
            "bytes=100-",
            "bytes=30-20",
            "bytes=-0",
            "bytes=-",
            "bytes=abc-2",
            "bytes=0-1,4-5",
            "bytes=0-1-2",
            "items=0-1",
            "bytes=+1-2",
            "bytes=0-18446744073709551616",
            "bytes=1 -2",
            "",
        ] {
            assert!(resolve_range(Some(header), 100).is_err(), "{header}");
        }
    }

    #[tokio::test]
    async fn cast_endpoint_ranges_acl_subtitles_and_revocation() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("video.mp4");
        tokio::fs::write(&file, b"0123456789").await.unwrap();
        let server = MediaServer::start().await.unwrap();
        let playback = server
            .register(MediaSource::Local(file), "video/mp4".into(), false)
            .await
            .unwrap();
        let subtitle = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHola\n".to_owned();
        let cast = server
            .cast_stream(
                &playback,
                std::net::Ipv4Addr::LOCALHOST,
                std::net::Ipv4Addr::LOCALHOST,
                false,
                Some(subtitle.clone()),
            )
            .await
            .unwrap();
        let response = request(&cast.url, "GET", "Range: bytes=2-5\r\n").await;
        let (headers, body) = split_response(&response);
        assert!(headers.starts_with("HTTP/1.1 206"));
        assert!(headers.contains("access-control-allow-origin: *"));
        assert_eq!(body, b"2345");
        let response = request(cast.subtitle_url.as_deref().unwrap(), "GET", "").await;
        assert_eq!(split_response(&response).1, subtitle.as_bytes());
        let url = reqwest::Url::parse(&cast.url).unwrap();
        let bad_token = format!(
            "http://{}:{}/wrong/video",
            url.host_str().unwrap(),
            url.port().unwrap()
        );
        assert!(split_response(&request(&bad_token, "GET", "").await)
            .0
            .starts_with("HTTP/1.1 404"));
        let denied = server
            .cast_stream(
                &playback,
                std::net::Ipv4Addr::LOCALHOST,
                std::net::Ipv4Addr::new(192, 168, 1, 5),
                false,
                None,
            )
            .await
            .unwrap();
        assert!(split_response(&request(&denied.url, "GET", "").await)
            .0
            .starts_with("HTTP/1.1 404"));
        server.clear().await;
        assert!(cast.cancel.is_cancelled());
        assert!(denied.cancel.is_cancelled());
        assert!(!hls_filename("../video.mp4"));
        assert!(!hls_filename("seg../secret.ts"));
        server.shutdown().await;
    }

    #[tokio::test]
    async fn cast_conversion_serves_real_hls_and_cleans_up() {
        let Some(ffmpeg) = find_ffmpeg().await else {
            return;
        };
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("cast.mkv");
        let generated = Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=160x90:rate=12",
                "-t",
                "1",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
            ])
            .arg(&file)
            .status()
            .await
            .unwrap();
        assert!(generated.success());
        let server = MediaServer::start().await.unwrap();
        let playback = server
            .register(MediaSource::Local(file), "video/x-matroska".into(), false)
            .await
            .unwrap();
        let cast = server
            .cast_stream(
                &playback,
                std::net::Ipv4Addr::LOCALHOST,
                std::net::Ipv4Addr::LOCALHOST,
                true,
                None,
            )
            .await
            .unwrap();
        let response = request(&cast.url, "GET", "").await;
        let (headers, body) = split_response(&response);
        assert!(headers.starts_with("HTTP/1.1 200"));
        let playlist = std::str::from_utf8(body).unwrap();
        assert!(playlist.starts_with("#EXTM3U"));
        let segment = playlist.lines().find(|line| line.ends_with(".ts")).unwrap();
        let segment_url = cast.url.replace("index.m3u8", segment);
        let segment_response = request(&segment_url, "GET", "").await;
        let (_, content) = split_response(&segment_response);
        assert!(!content.is_empty());
        assert_eq!(content[0], 0x47); // MPEG-TS packet synchronization byte
        let cancel = cast.cancel.clone();
        drop(cast);
        assert!(cancel.is_cancelled());
        server.shutdown().await;
    }

    async fn request(url: &str, method: &str, extra_headers: &str) -> Vec<u8> {
        let address_path = url.strip_prefix("http://").unwrap();
        let (address, path) = address_path.split_once('/').unwrap();
        let mut connection = tokio::net::TcpStream::connect(address).await.unwrap();
        connection
            .write_all(format!("{method} /{path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n{extra_headers}\r\n").as_bytes())
            .await
            .unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(5),
            connection.read_to_end(&mut response),
        )
        .await
        .unwrap()
        .unwrap();
        response
    }

    fn split_response(response: &[u8]) -> (&str, &[u8]) {
        let boundary = response
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
            .unwrap();
        (
            std::str::from_utf8(&response[..boundary]).unwrap(),
            &response[boundary + 4..],
        )
    }

    #[tokio::test]
    async fn loopback_serves_real_byte_ranges_and_head() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("video.mp4");
        tokio::fs::write(&file, b"0123456789").await.unwrap();
        let server = MediaServer::start().await.unwrap();
        let url = server
            .register(MediaSource::Local(file), "video/mp4".into(), false)
            .await
            .unwrap();

        let response = request(&url, "GET", "Range: bytes=3-6\r\n").await;
        let (headers, body) = split_response(&response);
        assert!(headers.starts_with("HTTP/1.1 206"));
        assert!(headers.contains("content-range: bytes 3-6/10"));
        assert!(headers.contains("content-length: 4"));
        assert_eq!(body, b"3456");

        let response = request(&url, "HEAD", "Range: bytes=-2\r\n").await;
        let (headers, body) = split_response(&response);
        assert!(headers.starts_with("HTTP/1.1 206"));
        assert!(headers.contains("content-range: bytes 8-9/10"));
        assert!(headers.contains("content-length: 2"));
        assert!(body.is_empty());

        let response = request(&url, "GET", "Range: bytes=10-\r\n").await;
        let (headers, _) = split_response(&response);
        assert!(headers.starts_with("HTTP/1.1 416"));
        assert!(headers.contains("content-range: bytes */10"));

        let response = request(&url, "GET", "").await;
        let (headers, body) = split_response(&response);
        assert!(headers.starts_with("HTTP/1.1 200"));
        assert!(headers.contains("content-type: video/mp4"));
        assert!(!headers.contains("access-control-allow-origin"));
        assert_eq!(body, b"0123456789");
        server.shutdown().await;
    }

    #[tokio::test]
    async fn loopback_requires_token_and_revokes_previous_sources() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("video.mp4");
        tokio::fs::write(&file, b"video").await.unwrap();
        let server = MediaServer::start().await.unwrap();
        let first = server
            .register(MediaSource::Local(file.clone()), "video/mp4".into(), false)
            .await
            .unwrap();
        let wrong_token = first.replace(&server.shared.token, "invalid-token");
        assert!(split_response(&request(&wrong_token, "GET", "").await)
            .0
            .starts_with("HTTP/1.1 404"));
        let next = server
            .register(MediaSource::Local(file), "video/mp4".into(), false)
            .await
            .unwrap();
        assert!(split_response(&request(&first, "GET", "").await)
            .0
            .starts_with("HTTP/1.1 404"));
        assert!(split_response(&request(&next, "GET", "").await)
            .0
            .starts_with("HTTP/1.1 200"));
        server.clear().await;
        assert!(split_response(&request(&next, "GET", "").await)
            .0
            .starts_with("HTTP/1.1 404"));
        server.shutdown().await;
    }

    #[tokio::test]
    async fn cors_allows_only_explicit_webview_origins() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("video.mp4");
        tokio::fs::write(&file, b"video").await.unwrap();
        let server = MediaServer::start().await.unwrap();
        let url = server
            .register(MediaSource::Local(file), "video/mp4".into(), false)
            .await
            .unwrap();
        for origin in [
            "tauri://localhost",
            "http://tauri.localhost",
            "https://tauri.localhost",
            "http://127.0.0.1:1420",
        ] {
            let response = request(&url, "GET", &format!("Origin: {origin}\r\n")).await;
            let (headers, body) = split_response(&response);
            assert!(headers.starts_with("HTTP/1.1 200"));
            assert!(headers.contains(&format!("access-control-allow-origin: {origin}")));
            assert!(headers.contains("vary: Origin"));
            assert!(!headers.contains("access-control-allow-credentials"));
            assert_eq!(body, b"video");
        }
        for origin in ["https://evil.example", "http://127.0.0.1:1421", "null"] {
            let response = request(&url, "GET", &format!("Origin: {origin}\r\n")).await;
            let (headers, _) = split_response(&response);
            assert!(!headers.contains("access-control-allow-origin"));
            assert!(!headers.contains("access-control-allow-credentials"));
        }
        server.clear().await;
        let response = request(&url, "GET", "Origin: tauri://localhost\r\n").await;
        let (headers, _) = split_response(&response);
        assert!(headers.starts_with("HTTP/1.1 404"));
        assert!(headers.contains("access-control-allow-origin: tauri://localhost"));
        server.shutdown().await;
    }

    #[tokio::test]
    async fn ffmpeg_converts_real_mkv_over_loopback_and_exits() {
        tokio::time::timeout(Duration::from_secs(20), async {
            let Some(ffmpeg) = find_ffmpeg().await else {
                eprintln!("SKIP: configure MUKURE_FFMPEG to run the real FFmpeg integration test");
                return;
            };
            let directory = tempfile::tempdir().unwrap();
            let file = directory.path().join("synthetic.mkv");
            // Matroska + PCM exercises a real container/audio conversion rather
            // than returning an already-compatible MP4 unchanged.
            let status = Command::new(&ffmpeg)
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-nostdin",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=160x90:rate=12",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:sample_rate=48000",
                    "-t",
                    "1",
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-pix_fmt",
                    "yuv420p",
                    "-c:a",
                    "pcm_s16le",
                ])
                .arg(&file)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .status()
                .await
                .expect("could not generate the synthetic video with FFmpeg");
            assert!(
                status.success(),
                "FFmpeg could not generate the synthetic MKV"
            );
            let server = MediaServer::start().await.unwrap();
            let url = server
                .register(MediaSource::Local(file), "video/x-matroska".into(), true)
                .await
                .unwrap();
            let response = request(&url, "GET", "").await;
            let (headers, body) = split_response(&response);
            assert!(headers.starts_with("HTTP/1.1 200"));
            assert!(headers.contains("content-type: video/mp4"));
            assert!(headers.contains("accept-ranges: none"));
            // The HTTP body may be chunked; these box types still occur in the
            // actual muxer output. mdat confirms media packets, not only headers.
            assert!(
                body.windows(4).any(|window| window == b"ftyp"),
                "missing MP4 file type box"
            );
            assert!(
                body.windows(4).any(|window| window == b"mdat"),
                "missing MP4 media data box"
            );
            // transcode holds this lock until child.wait() has completed. The
            // completed HTTP response must therefore leave no running process.
            let permit =
                tokio::time::timeout(Duration::from_secs(2), server.shared.ffmpeg_slot.lock())
                    .await
                    .expect("FFmpeg process did not exit after the response completed");
            drop(permit);
            server.clear().await;
            server.shutdown().await;
            directory.close().unwrap();
        })
        .await
        .expect("FFmpeg integration test exceeded 20 seconds");
    }
}

/// A receiver-scoped, temporary LAN endpoint. Dropping it revokes the URL and
/// terminates conversion; ordinary desktop playback remains loopback-only.
pub struct CastStream {
    pub url: String,
    pub content_type: String,
    pub subtitle_url: Option<String>,
    pub cancel: CancellationToken,
}
impl Drop for CastStream {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

struct CastHttpState {
    entry: Arc<MediaEntry>,
    token: String,
    receiver: std::net::IpAddr,
    cancel: CancellationToken,
    hls: Option<PathBuf>,
    subtitle: Option<String>,
}

impl MediaServer {
    pub async fn cast_stream(
        &self,
        playback_url: &str,
        local_ip: std::net::Ipv4Addr,
        receiver: std::net::Ipv4Addr,
        convert: bool,
        subtitle: Option<String>,
    ) -> Result<CastStream> {
        if let Some(text) = &subtitle {
            // Bound and validate data from the WebView before exposing a track.
            subtitles_for_cast(text)?;
        }
        let entry = self
            .shared
            .active
            .read()
            .await
            .as_ref()
            .filter(|entry| entry.playback_url == playback_url && !entry.cancel.is_cancelled())
            .cloned()
            .context("El video ya no está activo")?;
        let cancel = entry.cancel.child_token();
        let listener = TcpListener::bind((local_ip, 0))
            .await
            .context("No se pudo abrir el video en la red local")?;
        let token = Uuid::new_v4().simple().to_string();
        let base = format!(
            "http://{}:{}/{}",
            local_ip,
            listener.local_addr()?.port(),
            token
        );
        let subtitle_url = subtitle.as_ref().map(|_| format!("{base}/subtitles.vtt"));
        let hls_dir = if convert {
            Some(tempfile::Builder::new().prefix("mukure-cast-").tempdir()?)
        } else {
            None
        };
        let content_type = if convert {
            "application/x-mpegURL".to_owned()
        } else {
            entry.mime.to_str()?.to_owned()
        };
        let state = Arc::new(CastHttpState {
            entry: entry.clone(),
            token,
            receiver: receiver.into(),
            cancel: cancel.clone(),
            hls: hls_dir.as_ref().map(|d| d.path().to_owned()),
            subtitle,
        });
        let router = Router::new()
            .route("/{token}/{file}", axum::routing::any(cast_http))
            .with_state(state);
        let server_cancel = cancel.clone();
        tokio::spawn(async move {
            let serve = axum::serve(
                listener,
                router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(server_cancel.clone().cancelled_owned());
            let _ = serve.await;
        });
        let stream = CastStream {
            url: format!("{base}/{}", if convert { "index.m3u8" } else { "video" }),
            content_type,
            subtitle_url,
            cancel: cancel.clone(),
        };
        if let Some(directory) = hls_dir {
            let ffmpeg = self
                .ffmpeg_path()
                .context("FFmpeg no está disponible para convertir el video")?;
            let input = format!(
                "{}/{}/stream/{}",
                self.shared.origin, self.shared.token, entry.id
            );
            let mut child = Command::new(ffmpeg).args([
                "-hide_banner", "-loglevel", "error", "-nostdin", "-rw_timeout", "45000000",
                "-re", "-i", &input, "-map", "0:v:0", "-map", "0:a:0?", "-sn", "-dn",
                "-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-pix_fmt", "yuv420p",
                "-vf", "scale=w='min(1920,iw)':h='min(1080,ih)':force_original_aspect_ratio=decrease,pad=ceil(iw/2)*2:ceil(ih/2)*2",
                "-r", "30", "-profile:v", "high", "-level:v", "4.1", "-maxrate", "5M", "-bufsize", "10M",
                "-g", "120", "-keyint_min", "120", "-sc_threshold", "0",
                "-c:a", "aac", "-b:a", "160k", "-ac", "2",
                "-f", "hls", "-hls_time", "4", "-hls_list_size", "12", "-hls_delete_threshold", "2",
                "-hls_flags", "delete_segments+independent_segments+temp_file", "-hls_segment_filename", "seg%06d.ts", "index.m3u8",
            ]).current_dir(directory.path()).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
                .kill_on_drop(true).spawn().context("No se pudo convertir el video para Chromecast")?;
            let playlist = directory.path().join("index.m3u8");
            let process_cancel = cancel.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = process_cancel.cancelled() => { let _ = child.kill().await; let _ = child.wait().await; },
                    result = child.wait() => {
                        if !result.is_ok_and(|status| status.success()) { process_cancel.cancel(); }
                        // Keep a completed short video's final segments until Cast disconnects.
                        process_cancel.cancelled().await;
                    }
                }
                drop(directory);
            });
            let ready = async {
                loop {
                    if tokio::fs::metadata(&playlist).await.is_ok() {
                        return Ok(());
                    }
                    tokio::select! {
                        _ = cancel.cancelled() => bail!("Se interrumpió la conversión para Chromecast"),
                        _ = tokio::time::sleep(Duration::from_millis(200)) => {}
                    }
                }
            };
            tokio::time::timeout(Duration::from_secs(60), ready).await
                .context("No llegó video para convertir en 60 segundos. Revisá los peers o probá Directo")??;
        }
        Ok(stream)
    }
}

fn subtitles_for_cast(text: &str) -> Result<()> {
    crate::subtitles::parse("Chromecast.vtt".into(), text.as_bytes()).map(|_| ())
}

fn hls_filename(name: &str) -> bool {
    name == "index.m3u8"
        || name
            .strip_prefix("seg")
            .and_then(|s| s.strip_suffix(".ts"))
            .is_some_and(|digits| {
                !digits.is_empty()
                    && digits.len() <= 10
                    && digits.bytes().all(|b| b.is_ascii_digit())
            })
}

async fn cast_http(
    State(state): State<Arc<CastHttpState>>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    RoutePath((token, file)): RoutePath<(String, String)>,
    method: Method,
    headers: HeaderMap,
) -> Response<Body> {
    if peer.ip() != state.receiver || token != state.token {
        return empty_response(StatusCode::NOT_FOUND);
    }
    if state.cancel.is_cancelled() {
        return empty_response(StatusCode::GONE);
    }
    let mut response = if method == Method::OPTIONS {
        empty_response(StatusCode::NO_CONTENT)
    } else if method != Method::GET && method != Method::HEAD {
        empty_response(StatusCode::METHOD_NOT_ALLOWED)
    } else if file == "video" && state.hls.is_none() {
        direct_entry(state.entry.clone(), method.clone(), headers).await
    } else if file == "subtitles.vtt" {
        match &state.subtitle {
            Some(text) => cast_bytes(
                text.as_bytes().to_vec(),
                "text/vtt; charset=utf-8",
                method == Method::HEAD,
            ),
            None => empty_response(StatusCode::NOT_FOUND),
        }
    } else if let Some(directory) = &state.hls {
        if !hls_filename(&file) {
            return empty_response(StatusCode::NOT_FOUND);
        }
        match tokio::fs::read(directory.join(&file)).await {
            Ok(bytes) => cast_bytes(
                bytes,
                if file.ends_with(".m3u8") {
                    "application/x-mpegURL"
                } else {
                    "video/mp2t"
                },
                method == Method::HEAD,
            ),
            Err(_) => empty_response(StatusCode::NOT_FOUND),
        }
    } else {
        empty_response(StatusCode::NOT_FOUND)
    };
    // The Cast receiver runs from Google's origin; this port serves only the
    // selected receiver and unguessable session URLs, with no control routes.
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, HEAD, OPTIONS"),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("Range, Content-Type"),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("Content-Length, Content-Range, Accept-Ranges"),
    );
    let (parts, body) = response.into_parts();
    let mut data = body.into_data_stream();
    let cancel = state.cancel.clone();
    let stream = async_stream::stream! {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                chunk = data.next() => match chunk { Some(value) => yield value, None => break }
            }
        }
    };
    Response::from_parts(parts, Body::from_stream(stream))
}

fn cast_bytes(bytes: Vec<u8>, mime: &'static str, head: bool) -> Response<Body> {
    let mut response = empty_response(StatusCode::OK);
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&bytes.len().to_string()).unwrap(),
    );
    if !head {
        *response.body_mut() = Body::from(bytes);
    }
    response
}
