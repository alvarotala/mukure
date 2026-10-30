//! Bounded, local parsing of text subtitles. Rendering is delegated to the
//! WebView's VTTCue parser, never to HTML or an external subtitle service.

use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};
use serde::Serialize;
use tokio::io::AsyncReadExt;

pub const MAX_SUBTITLE_BYTES: u64 = 5 * 1024 * 1024;
const MAX_CUES: usize = 50_000;
const MAX_CUE_TEXT_BYTES: usize = 32 * 1024;

#[derive(Debug, Serialize)]
pub struct SubtitleCue {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct SubtitleTrack {
    pub name: String,
    pub cues: Vec<SubtitleCue>,
}

pub fn is_subtitle(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("srt") || value.eq_ignore_ascii_case("vtt"))
}

fn decode(bytes: &[u8]) -> Result<String> {
    let text = if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        let encoding = if bytes.starts_with(&[0xff, 0xfe]) {
            encoding_rs::UTF_16LE
        } else {
            encoding_rs::UTF_16BE
        };
        let (text, errors) = encoding.decode_without_bom_handling(&bytes[2..]);
        ensure!(!errors, "El subtítulo contiene texto UTF-16 inválido");
        text.into_owned()
    } else {
        let utf8_bom = bytes.starts_with(&[0xef, 0xbb, 0xbf]);
        let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
        match std::str::from_utf8(bytes) {
            Ok(text) => text.to_owned(),
            Err(_) if utf8_bom => bail!("El subtítulo contiene texto UTF-8 inválido"),
            // Many existing SRT files use Windows-1252 rather than UTF-8.
            Err(_) => {
                let (text, errors) = encoding_rs::WINDOWS_1252.decode_without_bom_handling(bytes);
                ensure!(!errors, "No se pudo decodificar el texto del subtítulo");
                text.into_owned()
            }
        }
    };
    ensure!(!text.contains('\0'), "El subtítulo contiene datos binarios");
    Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
}

fn integer(value: &str) -> Result<u64> {
    ensure!(
        !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()),
        "Marca de tiempo inválida"
    );
    value.parse().context("Marca de tiempo demasiado grande")
}

fn timestamp(value: &str, webvtt: bool) -> Result<f64> {
    let separator = if value.contains('.') { '.' } else { ',' };
    ensure!(
        !webvtt || separator == '.',
        "WebVTT requiere milisegundos con punto"
    );
    let (clock, fraction) = value
        .split_once(separator)
        .context("Faltan los milisegundos")?;
    ensure!(
        fraction.len() == 3,
        "La marca de tiempo requiere tres milisegundos"
    );
    let millis = integer(fraction)?;
    let parts: Vec<_> = clock.split(':').collect();
    ensure!(
        parts.len() == 3 || (webvtt && parts.len() == 2),
        "Formato de marca de tiempo inválido"
    );
    let (hours, minutes, seconds) = if parts.len() == 3 {
        ensure!(parts[0].len() >= 2, "La hora requiere al menos dos dígitos");
        (integer(parts[0])?, parts[1], parts[2])
    } else {
        (0, parts[0], parts[1])
    };
    ensure!(
        minutes.len() == 2 && seconds.len() == 2,
        "Minutos y segundos requieren dos dígitos"
    );
    let minutes = integer(minutes)?;
    let seconds = integer(seconds)?;
    ensure!(
        minutes < 60 && seconds < 60,
        "Minutos o segundos fuera de rango"
    );
    let time =
        hours as f64 * 3600.0 + minutes as f64 * 60.0 + seconds as f64 + millis as f64 / 1000.0;
    ensure!(time.is_finite(), "Marca de tiempo fuera de rango");
    Ok(time)
}

pub fn parse(name: String, bytes: &[u8]) -> Result<SubtitleTrack> {
    ensure!(is_subtitle(&name), "Selecciona un subtítulo .srt o .vtt");
    ensure!(
        bytes.len() as u64 <= MAX_SUBTITLE_BYTES,
        "El subtítulo supera los 5 MiB"
    );
    let text = decode(bytes)?;
    let text = text.trim_matches('\n');
    let webvtt = Path::new(&name)
        .extension()
        .and_then(|part| part.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("vtt"));
    let mut lines = text.lines().enumerate().peekable();
    if webvtt {
        let (_, header) = lines.next().context("El subtítulo está vacío")?;
        ensure!(
            header == "WEBVTT" || header.starts_with("WEBVTT ") || header.starts_with("WEBVTT\t"),
            "Falta la cabecera WEBVTT"
        );
        // Header metadata is separated from cues by the first blank line.
        for (_, line) in lines.by_ref() {
            if line.trim().is_empty() {
                break;
            }
            ensure!(
                !line.contains("-->"),
                "Falta una línea vacía después de WEBVTT"
            );
        }
    }
    let mut cues = Vec::new();
    while lines.peek().is_some() {
        let mut block = Vec::new();
        for (line_number, line) in lines.by_ref() {
            if line.trim().is_empty() {
                if block.is_empty() {
                    continue;
                }
                break;
            }
            block.push((line_number + 1, line));
        }
        if block.is_empty() {
            continue;
        }
        let first = block[0].1;
        if webvtt
            && (first == "NOTE"
                || first.starts_with("NOTE ")
                || first.starts_with("NOTE\t")
                || first == "STYLE"
                || first == "REGION")
        {
            continue;
        }
        let timing_index = if first.contains("-->") { 0 } else { 1 };
        let (line_number, timing) = block
            .get(timing_index)
            .context("El bloque de subtítulo no contiene una marca de tiempo")?;
        let cue = (|| -> Result<SubtitleCue> {
            let (start, right) = timing.split_once("-->").context("Falta el separador -->")?;
            let end = right
                .split_whitespace()
                .next()
                .context("Falta el tiempo final")?;
            let start = timestamp(start.trim(), webvtt)?;
            let end = timestamp(end, webvtt)?;
            ensure!(end > start, "El tiempo final debe ser posterior al inicio");
            let body: Vec<_> = block[timing_index + 1..]
                .iter()
                .map(|(_, line)| *line)
                .collect();
            ensure!(!body.is_empty(), "El subtítulo no contiene texto");
            ensure!(
                !body
                    .iter()
                    .any(|line| line.split_once("-->").is_some_and(|(start, end)| {
                        timestamp(start.trim(), webvtt).is_ok()
                            && end
                                .split_whitespace()
                                .next()
                                .is_some_and(|end| timestamp(end, webvtt).is_ok())
                    })),
                "Falta una línea vacía entre subtítulos"
            );
            let text = body.join("\n");
            ensure!(
                text.len() <= MAX_CUE_TEXT_BYTES,
                "El texto de un subtítulo es demasiado largo"
            );
            Ok(SubtitleCue { start, end, text })
        })()
        .with_context(|| format!("Subtítulo inválido en la línea {line_number}"))?;
        ensure!(
            cues.len() < MAX_CUES,
            "El archivo contiene demasiados subtítulos"
        );
        cues.push(cue);
    }
    ensure!(
        !cues.is_empty(),
        "No se encontraron subtítulos con tiempos válidos"
    );
    Ok(SubtitleTrack { name, cues })
}

pub async fn read_local(path: PathBuf) -> Result<SubtitleTrack> {
    ensure!(
        is_subtitle(&path.to_string_lossy()),
        "Selecciona un subtítulo .srt o .vtt"
    );
    let file = tokio::fs::File::open(&path)
        .await
        .context("No se pudo abrir el subtítulo")?;
    let metadata = file.metadata().await?;
    ensure!(metadata.is_file(), "El subtítulo debe ser un archivo");
    ensure!(
        metadata.len() <= MAX_SUBTITLE_BYTES,
        "El subtítulo supera los 5 MiB"
    );
    let mut bytes = Vec::new();
    file.take(MAX_SUBTITLE_BYTES + 1)
        .read_to_end(&mut bytes)
        .await?;
    let name = path
        .file_name()
        .context("El subtítulo no tiene nombre")?
        .to_string_lossy()
        .into_owned();
    parse(name, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bom_srt_crlf_accents_and_multiline() {
        let track = parse("Español.SRT".into(), "\u{feff}1\r\n00:00:01,250 --> 00:00:03,000\r\n¡Hola!\r\n<i>Qué tal</i>\r\n\r\n2\r\n00:00:02,000 --> 00:00:04,500\r\nSe permite superponer.\r\n".as_bytes()).unwrap();
        assert_eq!(track.cues.len(), 2);
        assert_eq!(track.cues[0].start, 1.25);
        assert_eq!(track.cues[0].end, 3.0);
        assert_eq!(track.cues[0].text, "¡Hola!\n<i>Qué tal</i>");
    }

    #[test]
    fn parses_vtt_identifiers_settings_and_skips_metadata() {
        let text = "WEBVTT Español\nLanguage: es\n\nNOTE commentary --> not a cue\nignore me\n\nSTYLE\n::cue { color: lime; }\n\nREGION\nid:main\n\nintro\n00:01.500 --> 00:04.000 align:start position:10%\nPrimera línea\n\n01:00:00.000 --> 01:00:01.000\nUna hora";
        let track = parse("test.vtt".into(), text.as_bytes()).unwrap();
        assert_eq!(track.cues.len(), 2);
        assert_eq!(track.cues[0].start, 1.5);
        assert_eq!(track.cues[1].start, 3600.0);
    }

    #[test]
    fn rejects_invalid_timing_empty_and_malformed_blocks() {
        for text in [
            "",
            "just some text",
            "1\n00:00:00,000 --> 00:00:01,000",
            "1\n00:60:00,000 --> 01:01:00,000\nBad",
            "1\n00:00:60,000 --> 00:01:01,000\nBad",
            "1\n00:00:01,1234 --> 00:00:02,000\nBad",
            "1\n00:00:02,000 --> 00:00:01,000\nBad",
            "1\n00:00:01,000 --> 00:00:01,000\nBad",
            "1\n-1:00:01,000 --> 00:00:02,000\nBad",
            "1\n00:00:00,000 --> 00:00:01,000\nA\n2\n00:00:02,000 --> 00:00:03,000\nB",
        ] {
            assert!(
                parse("bad.srt".into(), text.as_bytes()).is_err(),
                "accepted {text:?}"
            );
        }
        assert!(parse("bad.vtt".into(), b"00:00.000 --> 00:01.000\nNo header").is_err());
        assert!(parse(
            "bad.vtt".into(),
            b"WEBVTT\n00:00.000 --> 00:01.000\nNo separator"
        )
        .is_err());
    }

    #[test]
    fn decodes_windows_1252_and_utf16_in_both_byte_orders() {
        let text = "1\n00:00:00,000 --> 00:00:01,000\nEspañol – café";
        let (encoded, _, _) = encoding_rs::WINDOWS_1252.encode(text);
        assert_eq!(
            parse("legacy.srt".into(), &encoded).unwrap().cues[0].text,
            "Español – café"
        );
        for little in [true, false] {
            let mut bytes = if little {
                vec![0xff, 0xfe]
            } else {
                vec![0xfe, 0xff]
            };
            for word in text.encode_utf16() {
                bytes.extend_from_slice(&if little {
                    word.to_le_bytes()
                } else {
                    word.to_be_bytes()
                });
            }
            assert_eq!(
                parse("unicode.srt".into(), &bytes).unwrap().cues[0].text,
                "Español – café"
            );
        }
        assert!(parse("broken.srt".into(), &[0xff, 0xfe, 0x00]).is_err());
        assert!(parse("broken.srt".into(), &[0xef, 0xbb, 0xbf, 0xff]).is_err());
    }

    #[test]
    fn rejects_binary_unsupported_extension_and_excessive_input() {
        assert!(parse("binary.srt".into(), b"1\n00:00:00,000 --> 00:00:01,000\n\0").is_err());
        assert!(parse("other.ass".into(), b"").is_err());
        assert!(parse(
            "large.srt".into(),
            &vec![b' '; MAX_SUBTITLE_BYTES as usize + 1]
        )
        .is_err());
        let text = format!(
            "1\n00:00:00,000 --> 00:00:01,000\n{}",
            "a".repeat(MAX_CUE_TEXT_BYTES + 1)
        );
        assert!(parse("long.srt".into(), text.as_bytes()).is_err());
    }

    #[tokio::test]
    async fn reads_only_bounded_regular_subtitle_files() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("test.vtt");
        tokio::fs::write(&file, b"WEBVTT\n\n00:00.000 --> 00:01.000\nHello")
            .await
            .unwrap();
        assert_eq!(read_local(file.clone()).await.unwrap().name, "test.vtt");
        let large = tokio::fs::File::create(&file).await.unwrap();
        large.set_len(MAX_SUBTITLE_BYTES + 1).await.unwrap();
        assert!(read_local(file).await.is_err());
        let folder = root.path().join("directory.srt");
        tokio::fs::create_dir(&folder).await.unwrap();
        assert!(read_local(folder).await.is_err());
        assert!(read_local(root.path().join("missing.srt")).await.is_err());
    }
}
