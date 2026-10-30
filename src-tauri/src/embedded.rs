//! Embedded text subtitle discovery and incremental FFmpeg extraction.
//! Media URLs contain an ephemeral token and must never be logged or returned
//! in diagnostic messages. Processes stop when the selected source is revoked.

use std::{path::Path, process::Stdio, time::Duration};

use anyhow::{anyhow, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tokio::{io::AsyncReadExt, process::Command};
use tokio_util::sync::CancellationToken;

use crate::subtitles::{self, SubtitleCue};

const MAX_PROBE_BYTES: u64 = 256 * 1024;
const MAX_BLOCK_BYTES: usize = 128 * 1024;
const MAX_CUES: usize = 50_000;
const MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Serialize)]
pub struct EmbeddedTrack {
    pub id: usize,
    pub name: String,
    pub language: String,
    pub codec: String,
    pub supported: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SubtitleEvent {
    Cues { cues: Vec<SubtitleCue> },
    Done,
    Error { message: String },
}

#[derive(Deserialize)]
struct ProbeOutput {
    #[serde(default)]
    streams: Vec<ProbeStream>,
}

#[derive(Deserialize)]
struct ProbeStream {
    index: usize,
    #[serde(default)]
    codec_name: String,
    #[serde(default)]
    tags: ProbeTags,
}

#[derive(Default, Deserialize)]
struct ProbeTags {
    #[serde(default)]
    language: String,
    #[serde(default)]
    title: String,
}

fn parse_probe(bytes: &[u8]) -> Result<Vec<EmbeddedTrack>> {
    let output: ProbeOutput =
        serde_json::from_slice(bytes).context("FFprobe devolvió una lista de pistas inválida")?;
    ensure!(
        output.streams.len() <= 256,
        "El video contiene demasiadas pistas de subtítulos"
    );
    Ok(output
        .streams
        .into_iter()
        .enumerate()
        .map(|(number, stream)| {
            let supported = matches!(
                stream.codec_name.as_str(),
                "subrip"
                    | "srt"
                    | "ass"
                    | "ssa"
                    | "mov_text"
                    | "webvtt"
                    | "text"
                    | "microdvd"
                    | "mpl2"
                    | "jacosub"
                    | "sami"
                    | "realtext"
                    | "subviewer"
                    | "subviewer1"
                    | "vplayer"
                    | "pjs"
            );
            let title = stream.tags.title.trim();
            let language = stream.tags.language.trim();
            let name = match (title.is_empty(), language.is_empty() || language == "und") {
                (false, false) => format!("{title} ({language})"),
                (false, true) => title.to_owned(),
                (true, false) => language.to_owned(),
                (true, true) => format!("Pista {}", number + 1),
            };
            EmbeddedTrack {
                id: stream.index,
                name,
                language: language.to_owned(),
                codec: stream.codec_name,
                supported,
            }
        })
        .collect())
}

pub async fn probe(
    ffprobe: &Path,
    input: &str,
    cancel: CancellationToken,
) -> Result<Vec<EmbeddedTrack>> {
    let mut child = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-rw_timeout",
            "20000000",
            "-select_streams",
            "s",
            "-show_entries",
            "stream=index,codec_name:stream_tags=language,title",
            "-of",
            "json",
            input,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("No se pudo iniciar FFprobe")?;
    let stdout = child.stdout.take().context("FFprobe no tiene salida")?;
    let result = tokio::select! {
        _ = cancel.cancelled() => Err(anyhow!("Lectura de pistas cancelada")),
        result = tokio::time::timeout(Duration::from_secs(20), async {
            let mut bytes = Vec::new();
            stdout.take(MAX_PROBE_BYTES + 1).read_to_end(&mut bytes).await?;
            ensure!(bytes.len() as u64 <= MAX_PROBE_BYTES, "La lista de pistas es demasiado grande");
            let status = child.wait().await?;
            ensure!(status.success(), "No se pudieron leer las pistas de este video");
            parse_probe(&bytes)
        }) => match result {
            Ok(result) => result,
            Err(_) => Err(anyhow!("Se agotó el tiempo al leer las pistas del video")),
        },
    };
    if result.is_err() {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    result
}

/// FFmpeg emits complete WebVTT cue blocks, but the pipe may split any UTF-8
/// character or separator. Keep bytes until a complete block is available.
#[derive(Default)]
struct CueDecoder {
    pending: Vec<u8>,
    header: bool,
    cues: usize,
    text_bytes: usize,
}

impl CueDecoder {
    fn push(&mut self, bytes: &[u8], eof: bool) -> Result<Vec<SubtitleCue>> {
        self.pending.extend_from_slice(bytes);
        let mut cues = Vec::new();
        while let Some(end) = self.pending.windows(2).position(|pair| pair == b"\n\n") {
            ensure!(
                end <= MAX_BLOCK_BYTES,
                "Un subtítulo incrustado es demasiado largo"
            );
            let block = self.pending.drain(..end + 2).collect::<Vec<_>>();
            cues.extend(self.block(&block[..end])?);
        }
        ensure!(
            self.pending.len() <= MAX_BLOCK_BYTES,
            "Un subtítulo incrustado es demasiado largo"
        );
        if eof && !self.pending.is_empty() {
            let block = std::mem::take(&mut self.pending);
            cues.extend(self.block(&block)?);
        }
        if eof {
            ensure!(
                self.header,
                "FFmpeg no generó una cabecera de subtítulos válida"
            );
        }
        Ok(cues)
    }

    fn block(&mut self, bytes: &[u8]) -> Result<Vec<SubtitleCue>> {
        let block = std::str::from_utf8(bytes)
            .context("FFmpeg generó subtítulos UTF-8 inválidos")?
            .trim_matches('\n');
        if block.trim().is_empty() {
            return Ok(Vec::new());
        }
        if !self.header {
            ensure!(
                block.lines().next() == Some("WEBVTT"),
                "FFmpeg no generó subtítulos WebVTT"
            );
            self.header = true;
            return Ok(Vec::new());
        }
        let first = block.lines().next().unwrap_or_default();
        if first == "NOTE" || first.starts_with("NOTE ") || first == "STYLE" || first == "REGION" {
            return Ok(Vec::new());
        }
        let document = format!("WEBVTT\n\n{block}");
        let track = subtitles::parse("embedded.vtt".into(), document.as_bytes())?;
        self.cues += track.cues.len();
        self.text_bytes += track.cues.iter().map(|cue| cue.text.len()).sum::<usize>();
        ensure!(
            self.cues <= MAX_CUES && self.text_bytes <= MAX_TEXT_BYTES,
            "La pista incrustada contiene demasiados subtítulos"
        );
        Ok(track.cues)
    }
}

/// `deliver` returning false means the consumer no longer exists; cancel the
/// child immediately instead of scanning the rest of a movie without a viewer.
async fn extract_to(
    ffmpeg: &Path,
    input: &str,
    track_id: usize,
    start_time: f64,
    cancel: CancellationToken,
    mut deliver: impl FnMut(SubtitleEvent) -> bool,
) -> Result<()> {
    ensure!(
        start_time.is_finite() && start_time >= 0.0,
        "La posición de los subtítulos debe ser un tiempo válido"
    );
    // Input seeking avoids scanning all preceding torrent pieces. Keep a
    // preroll window for cues that started before the current playback time.
    // Output seeking would subtract this offset and desynchronize the cues.
    let seek = (start_time - 30.0).max(0.0).to_string();
    let map = format!("0:{track_id}");
    let mut child = Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-rw_timeout",
            "45000000",
            "-ss",
            &seek,
            "-copyts",
            "-start_at_zero",
            "-i",
            input,
            "-map",
            &map,
            "-c:s",
            "webvtt",
            "-f",
            "webvtt",
            "-flush_packets",
            "1",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("No se pudo iniciar FFmpeg para leer los subtítulos")?;
    let mut stdout = child
        .stdout
        .take()
        .context("FFmpeg no tiene salida de subtítulos")?;
    let result = async {
        let mut decoder = CueDecoder::default();
        let mut chunk = [0; 16 * 1024];
        loop {
            let count = tokio::select! {
                _ = cancel.cancelled() => return Ok(()),
                read = tokio::time::timeout(Duration::from_secs(45), stdout.read(&mut chunk)) =>
                    read.context("Se agotó el tiempo esperando subtítulos del video")??,
            };
            let cues = decoder.push(&chunk[..count], count == 0)?;
            if !cues.is_empty() && !deliver(SubtitleEvent::Cues { cues }) {
                return Ok(());
            }
            if count == 0 {
                break;
            }
        }
        let status = tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            value = tokio::time::timeout(Duration::from_secs(5), child.wait()) =>
                value.context("FFmpeg no terminó de leer los subtítulos")??,
        };
        ensure!(
            status.success(),
            "FFmpeg no pudo extraer esta pista de subtítulos"
        );
        deliver(SubtitleEvent::Done);
        Ok(())
    }
    .await;
    // This also handles cancellation or a channel that closed before EOF.
    let _ = child.kill().await;
    let _ = child.wait().await;
    result
}

pub async fn extract(
    ffmpeg: &Path,
    input: &str,
    track_id: usize,
    start_time: f64,
    cancel: CancellationToken,
    on_event: Channel<SubtitleEvent>,
) -> Result<()> {
    extract_to(ffmpeg, input, track_id, start_time, cancel, |event| {
        on_event.send(event).is_ok()
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_named_text_and_unsupported_bitmap_tracks() {
        let tracks = parse_probe(r#"{"streams":[{"index":2,"codec_name":"subrip","tags":{"language":"spa","title":"Español"}},{"index":4,"codec_name":"hdmv_pgs_subtitle"}]}"#.as_bytes()).unwrap();
        assert_eq!(tracks[0].id, 2);
        assert_eq!(tracks[0].name, "Español (spa)");
        assert!(tracks[0].supported);
        assert_eq!(tracks[1].name, "Pista 2");
        assert!(!tracks[1].supported);
        assert!(parse_probe(b"not json").is_err());
    }

    #[test]
    fn incremental_decoder_preserves_split_utf8_and_emits_before_eof() {
        let text = "WEBVTT\n\n00:00.500 --> 00:02.000\n¡Hola!\nLínea dos\n\n00:03.000 --> 00:04.000\nAdiós";
        let mut decoder = CueDecoder::default();
        let mut cues = Vec::new();
        for byte in text.as_bytes() {
            cues.extend(decoder.push(&[*byte], false).unwrap());
        }
        assert_eq!(cues.len(), 1, "first cue must arrive before pipe closes");
        cues.extend(decoder.push(&[], true).unwrap());
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "¡Hola!\nLínea dos");
        assert_eq!(cues[1].start, 3.0);
    }

    #[test]
    fn incremental_decoder_rejects_malformed_truncated_and_oversized_output() {
        for text in [
            b"garbage\n\n".as_slice(),
            b"WEBVTT\n\n00:00.500 -->".as_slice(),
            b"WEBVTT\n\n00:02.000 --> 00:01.000\nBad".as_slice(),
        ] {
            assert!(CueDecoder::default().push(text, true).is_err());
        }
        assert!(CueDecoder::default()
            .push(&vec![b'x'; MAX_BLOCK_BYTES + 1], false)
            .is_err());
        assert!(CueDecoder::default().push(b"", true).is_err());
        assert!(CueDecoder::default()
            .push(b"WEBVTT\n\n", true)
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn extracts_real_embedded_cues_and_preserves_media_timestamps() -> Result<()> {
        let Some(ffmpeg) = std::env::var_os("MUKURE_FFMPEG") else {
            eprintln!("Skipped real embedded extraction: MUKURE_FFMPEG is unavailable");
            return Ok(());
        };
        let ffmpeg = Path::new(&ffmpeg);
        let root = tempfile::tempdir()?;
        let subtitle = root.path().join("fixture.srt");
        let video = root.path().join("fixture.mkv");
        tokio::fs::write(&subtitle, b"1\n00:00:00,500 --> 00:00:01,500\nFirst cue\n\n2\n00:00:02,000 --> 00:00:02,750\nSecond cue\n").await?;
        let status = Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=size=160x90:rate=10",
                "-i",
            ])
            .arg(&subtitle)
            .args([
                "-t",
                "3",
                "-map",
                "0:v",
                "-map",
                "1:s",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-c:s",
                "srt",
                "-metadata:s:s:0",
                "language=eng",
            ])
            .arg(&video)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status()
            .await?;
        ensure!(status.success(), "could not create subtitle fixture");
        if let Some(ffprobe) = std::env::var_os("MUKURE_FFPROBE") {
            let tracks = probe(
                Path::new(&ffprobe),
                video.to_str().unwrap(),
                CancellationToken::new(),
            )
            .await?;
            ensure!(tracks.len() == 1 && tracks[0].id == 1);
            ensure!(tracks[0].language == "eng" && tracks[0].supported);
        }
        let mut cues = Vec::new();
        let mut done = false;
        tokio::time::timeout(
            Duration::from_secs(20),
            extract_to(
                ffmpeg,
                video.to_str().unwrap(),
                1,
                0.0,
                CancellationToken::new(),
                |event| {
                    match event {
                        SubtitleEvent::Cues { cues: batch } => cues.extend(batch),
                        SubtitleEvent::Done => done = true,
                        SubtitleEvent::Error { .. } => unreachable!(),
                    }
                    true
                },
            ),
        )
        .await??;
        ensure!(done && cues.len() == 2);
        ensure!(cues[0].start == 0.5 && cues[0].end == 1.5 && cues[1].start == 2.0);
        ensure!(cues[1].text == "Second cue");
        Ok(())
    }

    #[tokio::test]
    async fn input_seek_keeps_absolute_embedded_cue_times() -> Result<()> {
        let Some(ffmpeg) = std::env::var_os("MUKURE_FFMPEG") else {
            eprintln!("Skipped embedded seek extraction: MUKURE_FFMPEG is unavailable");
            return Ok(());
        };
        let ffmpeg = Path::new(&ffmpeg);
        let root = tempfile::tempdir()?;
        let subtitle = root.path().join("seek.srt");
        let video = root.path().join("seek.mkv");
        tokio::fs::write(&subtitle, b"1\n00:00:01,000 --> 00:00:02,000\nEarlier\n\n2\n00:00:42,000 --> 00:00:44,000\nAfter seeking\n").await?;
        let status = Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=size=160x90:rate=2",
                "-i",
            ])
            .arg(&subtitle)
            .args([
                "-t",
                "45",
                "-map",
                "0:v",
                "-map",
                "1:s",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-g",
                "2",
                "-c:s",
                "srt",
            ])
            .arg(&video)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status()
            .await?;
        ensure!(status.success(), "could not create seek subtitle fixture");
        let mut cues = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(20),
            extract_to(
                ffmpeg,
                video.to_str().unwrap(),
                1,
                40.0,
                CancellationToken::new(),
                |event| {
                    if let SubtitleEvent::Cues { cues: batch } = event {
                        cues.extend(batch);
                    }
                    true
                },
            ),
        )
        .await??;
        let cue = cues
            .iter()
            .find(|cue| cue.text == "After seeking")
            .context("missing seeked cue")?;
        ensure!(
            cue.start == 42.0 && cue.end == 44.0,
            "input seeking shifted subtitle timestamps"
        );
        for invalid in [f64::NAN, f64::INFINITY, -1.0] {
            ensure!(extract_to(
                ffmpeg,
                video.to_str().unwrap(),
                1,
                invalid,
                CancellationToken::new(),
                |_| true
            )
            .await
            .is_err());
        }
        Ok(())
    }
}
