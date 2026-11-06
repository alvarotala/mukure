//! Account-free subtitle lookups. Only a basename/title leaves the computer;
//! media URLs, local paths and video bytes are never sent to these services.
use crate::subtitles::{self, SubtitleTrack, MAX_SUBTITLE_BYTES};
use anyhow::{bail, ensure, Context, Result};
use futures_util::StreamExt;
use regex::Regex;
use reqwest::{Client, Url};
use serde::Serialize;
use serde_json::Value;
use std::{collections::HashSet, time::Duration};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OnlineSubtitle {
    pub id: String,
    pub provider: String,
    pub name: String,
    pub language: String,
    pub url: String,
    pub format: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub title: String,
    pub subtitles: Vec<OnlineSubtitle>,
}

#[derive(Debug)]
struct Identity {
    release: String,
    title: String,
    year: Option<String>,
    episode: Option<(u32, u32)>,
    imdb: Option<String>,
}

fn normalized(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn identify(input: &str) -> Result<Identity> {
    ensure!(input.len() <= 512, "El nombre del video es demasiado largo");
    let basename = input.rsplit(['/', '\\']).next().unwrap_or(input).trim();
    let extension = Regex::new(r"(?i)\.(mp4|mkv|mov|avi|m4v|webm|ts|m2ts|mpg|mpeg|ogv)$")?;
    let release = extension.replace(basename, "").to_string();
    let episode_re = Regex::new(r"(?i)\b(?:s(\d{1,2})[ ._-]*e(\d{1,3})|(\d{1,2})x(\d{1,3}))\b")?;
    let clean = release.replace(['.', '_'], " ");
    let captures = episode_re.captures(&clean);
    let episode = captures.as_ref().map(|c| {
        (
            c.get(1).or(c.get(3)).unwrap().as_str().parse().unwrap(),
            c.get(2).or(c.get(4)).unwrap().as_str().parse().unwrap(),
        )
    });
    let year_re = Regex::new(r"\b(?:19|20)\d{2}\b")?;
    // A numeric title such as 1917 must not be mistaken for a release year.
    let year_match = year_re.find_iter(&clean).find(|m| m.start() > 0);
    let cut = [captures.as_ref().map(|c| c.get(0).unwrap().start()), year_match.as_ref().map(|m| m.start()),
        Regex::new(r"(?i)\b(?:2160p|1080[pi]|720p|480p|bluray|blu-ray|brrip|bdrip|webrip|web-dl|hdtv|dvdrip|x264|x265|h264|h265|hevc)\b")?.find(&clean).map(|m| m.start())]
        .into_iter().flatten().min().unwrap_or(clean.len());
    let title = clean[..cut]
        .trim_matches(|c: char| c.is_whitespace() || "([{-".contains(c))
        .trim()
        .to_owned();
    let imdb = Regex::new(r"\btt\d{7,10}\b")?
        .find(&release)
        .map(|m| m.as_str().to_owned());
    ensure!(
        !title.is_empty(),
        "Escribí el título o un ID de IMDb para buscar"
    );
    Ok(Identity {
        release,
        title,
        year: year_match.map(|m| m.as_str().to_owned()),
        episode,
        imdb,
    })
}

fn allowed_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && matches!(
            url.host_str(),
            Some(
                "api.subdl.com"
                    | "dl.subdl.com"
                    | "sub.wyzie.io"
                    | "api.thesubtitledb.org"
                    | "opensubtitles-v3.strem.io"
                    | "v3-cinemeta.strem.io"
                    | "subs1.strem.io"
                    | "subs2.strem.io"
                    | "subs3.strem.io"
                    | "subs4.strem.io"
                    | "subs5.strem.io"
            )
        )
}

fn client() -> Result<Client> {
    Ok(Client::builder()
        .user_agent(concat!("Mukure/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() < 3 && allowed_url(attempt.url()) {
                attempt.follow()
            } else {
                attempt.error("Destino de subtítulos no permitido")
            }
        }))
        .build()?)
}

async fn bytes(client: &Client, url: Url, limit: u64) -> Result<Vec<u8>> {
    ensure!(allowed_url(&url), "Destino de subtítulos no permitido");
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("La fuente no respondió. Podés reintentar la búsqueda"))?;
    match response.status().as_u16() {
        401 | 403 => {
            bail!("Clave inválida, vencida o acceso rechazado. Revisá la conexión del proveedor")
        }
        429 => bail!("Se alcanzó el límite de consultas del proveedor. Reintentá más tarde"),
        _ => ensure!(
            response.status().is_success(),
            "La fuente respondió HTTP {}",
            response.status().as_u16()
        ),
    }
    ensure!(
        response.content_length().unwrap_or(0) <= limit,
        "La respuesta supera el límite permitido"
    );
    let mut stream = response.bytes_stream();
    let mut data = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|_| anyhow::anyhow!("Se interrumpió la descarga de subtítulos"))?;
        ensure!(
            data.len() as u64 + chunk.len() as u64 <= limit,
            "La respuesta supera el límite permitido"
        );
        data.extend_from_slice(&chunk);
    }
    Ok(data)
}

async fn json(client: &Client, url: Url) -> Result<Value> {
    serde_json::from_slice(&bytes(client, url, 2 * 1024 * 1024).await?)
        .context("La fuente devolvió una respuesta inválida")
}

fn string<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
fn language_matches(actual: &str, language: &str) -> bool {
    match language {
        "es" => matches!(actual, "es" | "ea" | "spa" | "spn" | "spl"),
        "en" => matches!(actual, "en" | "eng"),
        "pt" => matches!(actual, "pt" | "pb" | "por" | "pob"),
        "fr" => matches!(actual, "fr" | "fra" | "fre"),
        "it" => matches!(actual, "it" | "ita"),
        "de" => matches!(actual, "de" | "ger" | "deu"),
        _ => false,
    }
}

async fn resolve_imdb(client: &Client, identity: &Identity) -> Result<String> {
    let kind = if identity.episode.is_some() {
        "series"
    } else {
        "movie"
    };
    let imdb = if let Some(id) = &identity.imdb {
        id.clone()
    } else {
        let mut url = Url::parse("https://v3-cinemeta.strem.io")?;
        url.path_segments_mut().unwrap().extend([
            "catalog",
            kind,
            "top",
            &format!("search={}.json", identity.title),
        ]);
        let metadata = json(client, url).await?;
        let matches: Vec<_> = metadata["metas"]
            .as_array()
            .context("No se pudo identificar el video")?
            .iter()
            .filter(|m| normalized(string(m, "name")) == normalized(&identity.title))
            .filter(|m| {
                identity
                    .year
                    .as_ref()
                    .is_none_or(|year| string(m, "releaseInfo").starts_with(year))
            })
            .collect();
        ensure!(matches.len() == 1, "No se pudo identificar el título con certeza. Probá título y año, o un ID IMDb (tt…) con S01E01 para series");
        string(matches[0], "id").to_owned()
    };
    ensure!(
        Regex::new(r"^tt\d{7,10}$")?.is_match(&imdb),
        "ID de IMDb inválido"
    );
    Ok(imdb)
}

async fn opensubtitles(
    client: &Client,
    identity: &Identity,
    language: &str,
) -> Result<SearchResult> {
    let kind = if identity.episode.is_some() {
        "series"
    } else {
        "movie"
    };
    let imdb = resolve_imdb(client, identity).await?;
    let id = match identity.episode {
        Some((s, e)) => format!("{imdb}:{s}:{e}"),
        None => imdb,
    };
    let mut url = Url::parse("https://opensubtitles-v3.strem.io")?;
    url.path_segments_mut()
        .unwrap()
        .extend(["subtitles", kind, &format!("{id}.json")]);
    let data = json(client, url).await?;
    let rows = data["subtitles"]
        .as_array()
        .context("OpenSubtitles devolvió una respuesta inválida")?;
    let subtitles = rows
        .iter()
        .filter(|row| language_matches(string(row, "lang"), language))
        .filter(|row| {
            identity.episode.is_none_or(|(s, e)| {
                // Some responses omit episode metadata; when present it must match.
                row["season"].as_u64().is_none_or(|v| v == s as u64)
                    && row["episode"].as_u64().is_none_or(|v| v == e as u64)
            })
        })
        .filter_map(|row| {
            let url = string(row, "url");
            if !Url::parse(url).is_ok_and(|u| allowed_url(&u)) {
                return None;
            }
            let name = string(row, "subtitleFileName");
            Some(OnlineSubtitle {
                id: format!("opensubtitles:{}", string(row, "id")),
                provider: "OpenSubtitles".into(),
                name: if name.is_empty() {
                    identity.release.clone()
                } else {
                    name.to_owned()
                },
                language: string(row, "lang").into(),
                url: url.into(),
                format: "srt".into(),
            })
        })
        .take(40)
        .collect();
    Ok(SearchResult {
        title: identity.release.clone(),
        subtitles,
    })
}

async fn subtitledb(client: &Client, identity: &Identity, language: &str) -> Result<SearchResult> {
    let suffix = identity
        .episode
        .map(|(s, e)| format!("/season/{s}/episode/{e}"))
        .unwrap_or_default();
    let mut url = Url::parse(&match &identity.imdb {
        Some(id) => format!("https://api.thesubtitledb.org/v1/by-imdb/{id}{suffix}"),
        None => format!("https://api.thesubtitledb.org/v1/by-releasename{suffix}"),
    })?;
    let langs = match language {
        "es" => "es,ea",
        "pt" => "pt,pb",
        other => other,
    };
    url.query_pairs_mut().extend_pairs([
        ("lang", langs),
        ("format", "srt"),
        ("limit", "40"),
        ("client", "mukure"),
    ]);
    if identity.imdb.is_none() {
        url.query_pairs_mut()
            .append_pair("release", &identity.release);
    }
    let data = json(client, url).await?;
    let title = string(&data["title"], "name");
    ensure!(!title.is_empty(), "SubtitleDB no reconoció el título");
    if identity.imdb.is_none() {
        ensure!(
            normalized(title) == normalized(&identity.title),
            "SubtitleDB encontró otro título ({title}). Corregí la búsqueda o usá un ID IMDb"
        );
        if let Some(year) = &identity.year {
            ensure!(
                data["title"]["year"]
                    .as_u64()
                    .map(|v| v.to_string())
                    .as_ref()
                    == Some(year),
                "El año encontrado no coincide con el video"
            );
        }
    }
    let rows = data["subtitles"]["items"]
        .as_array()
        .context("SubtitleDB devolvió una respuesta inválida")?;
    let subtitles = rows
        .iter()
        .filter(|row| language_matches(string(row, "language"), language))
        .filter_map(|row| {
            let id = row["id"].as_u64()?;
            let name = string(row, "release_name").trim();
            Some(OnlineSubtitle {
                id: format!("subtitledb:{id}"),
                provider: "SubtitleDB".into(),
                name: if name.is_empty() {
                    title.into()
                } else {
                    name.into()
                },
                language: string(row, "language").into(),
                url: format!("https://api.thesubtitledb.org/get/{id}?client=mukure"),
                format: "srt".into(),
            })
        })
        .take(40)
        .collect();
    Ok(SearchResult {
        title: title.into(),
        subtitles,
    })
}

pub async fn search(
    provider: &str,
    query: &str,
    language: &str,
    key: Option<&str>,
) -> Result<SearchResult> {
    ensure!(
        matches!(language, "es" | "en" | "pt" | "fr" | "it" | "de"),
        "Idioma no compatible"
    );
    let identity = identify(query)?;
    let client = client()?;
    let mut result = match provider {
        "opensubtitles" => opensubtitles(&client, &identity, language).await?,
        "subtitledb" => subtitledb(&client, &identity, language).await?,
        "subdl" => {
            subdl(
                &client,
                &identity,
                language,
                key.context("Conectá SubDL en Fuentes")?,
            )
            .await?
        }
        "wyzie" => {
            wyzie(
                &client,
                &identity,
                language,
                key.context("Conectá Wyzie en Fuentes")?,
            )
            .await?
        }
        _ => bail!("Fuente de subtítulos desconocida"),
    };
    let mut seen = HashSet::new();
    result
        .subtitles
        .retain(|item| seen.insert(item.url.clone()));
    // Prefer the same release when the provider knows its filename.
    let release = normalized(&identity.release);
    result
        .subtitles
        .sort_by_key(|item| std::cmp::Reverse(normalized(&item.name).contains(&release)));
    Ok(result)
}

pub async fn download(url: &str, name: &str) -> Result<SubtitleTrack> {
    let url = Url::parse(url)?;
    ensure!(
        allowed_url(&url)
            && (url.host_str() == Some("api.thesubtitledb.org") && url.path().starts_with("/get/")
                || url.host_str() == Some("dl.subdl.com") && url.path().starts_with("/subtitle/")
                || url.host_str() == Some("sub.wyzie.io") && url.path().starts_with("/c/")
                || url.host_str().is_some_and(|h| h.starts_with("subs"))
                    && url.path().contains("/download/")),
        "Enlace de descarga no permitido"
    );
    let data = bytes(&client()?, url, MAX_SUBTITLE_BYTES).await?;
    let name = name.rsplit(['/', '\\']).next().unwrap_or("Subtítulos");
    ensure!(name.len() <= 512, "Nombre de subtítulos inválido");
    let filename = if subtitles::is_subtitle(name) {
        name.to_owned()
    } else {
        format!("{name}.srt")
    };
    if data.starts_with(b"PK\x03\x04") {
        unpack_subtitle(&data, name)
    } else {
        subtitles::parse(filename, &data)
    }
}

fn unpack_subtitle(data: &[u8], expected: &str) -> Result<SubtitleTrack> {
    use std::io::{Cursor, Read};
    let mut archive = zip::ZipArchive::new(Cursor::new(data)).context("Archivo ZIP inválido")?;
    ensure!(archive.len() <= 100, "El ZIP contiene demasiados archivos");
    let expected_episode = identify(expected).ok().and_then(|i| i.episode);
    let mut candidates = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let name = entry.name()?.into_owned();
        if subtitles::is_subtitle(&name) && !entry.is_dir() {
            candidates.push((index, name));
        }
    }
    if candidates.len() > 1 {
        candidates.retain(|(_, name)| {
            expected_episode.is_some()
                && identify(name).ok().and_then(|i| i.episode) == expected_episode
        });
    }
    ensure!(
        candidates.len() == 1,
        "El paquete contiene varias pistas. Elegí un subtítulo individual para este episodio"
    );
    let (index, name) = &candidates[0];
    let mut entry = archive.by_index(*index)?;
    ensure!(
        entry.size() <= MAX_SUBTITLE_BYTES,
        "El subtítulo supera los 5 MiB"
    );
    let mut content = Vec::new();
    (&mut entry)
        .take(MAX_SUBTITLE_BYTES + 1)
        .read_to_end(&mut content)?;
    subtitles::parse(name.clone(), &content)
}

async fn subdl(
    client: &Client,
    identity: &Identity,
    language: &str,
    key: &str,
) -> Result<SearchResult> {
    let mut url = Url::parse("https://api.subdl.com/api/v1/subtitles")?;
    url.query_pairs_mut().extend_pairs([
        ("api_key", key),
        ("file_name", &identity.release),
        ("languages", &language.to_uppercase()),
        ("subs_per_page", "30"),
        ("unpack", "1"),
        ("full_season", "0"),
        ("client", "custom_integration"),
    ]);
    if let Some(imdb) = &identity.imdb {
        url.query_pairs_mut().append_pair("imdb_id", imdb);
    }
    if let Some(year) = &identity.year {
        url.query_pairs_mut().append_pair("year", year);
    }
    if let Some((s, e)) = identity.episode {
        url.query_pairs_mut().extend_pairs([
            ("type", "tv"),
            ("season_number", &s.to_string()),
            ("episode_number", &e.to_string()),
        ]);
    } else {
        url.query_pairs_mut().append_pair("type", "movie");
    }
    let data = json(client, url).await?;
    ensure!(
        data["status"] == true,
        "SubDL rechazó la búsqueda. Revisá tu clave o probá otro título"
    );
    parse_subdl(&data, identity, language)
}

fn parse_subdl(data: &Value, identity: &Identity, language: &str) -> Result<SearchResult> {
    let mut subtitles = Vec::new();
    for row in data["subtitles"]
        .as_array()
        .context("Respuesta de SubDL inválida")?
    {
        let unpacked = row["unpack_files"].as_array();
        let entries: Vec<&Value> = match unpacked {
            Some(files) if !files.is_empty() => files.iter().collect(),
            _ => vec![row],
        };
        for file in entries {
            if let Some((s, e)) = identity.episode {
                if file["season"].as_u64().is_some_and(|v| v != s as u64)
                    || file["episode"].as_u64().is_some_and(|v| v != e as u64)
                {
                    continue;
                }
            }
            let format = string(file, "format");
            if !format.is_empty() && !matches!(format, "srt" | "vtt") {
                continue;
            }
            let url = Url::parse("https://dl.subdl.com")?.join(string(file, "url"))?;
            if url.host_str() != Some("dl.subdl.com") || !url.path().starts_with("/subtitle/") {
                continue;
            }
            let name = string(file, "name");
            subtitles.push(OnlineSubtitle {
                id: format!("subdl:{}", url.path()),
                provider: "SubDL".into(),
                name: if name.is_empty() {
                    identity.release.clone()
                } else {
                    name.into()
                },
                language: language.into(),
                url: url.into(),
                format: if format.is_empty() { "srt" } else { format }.into(),
            });
        }
    }
    Ok(SearchResult {
        title: identity.release.clone(),
        subtitles,
    })
}

async fn wyzie(
    client: &Client,
    identity: &Identity,
    language: &str,
    key: &str,
) -> Result<SearchResult> {
    let imdb = resolve_imdb(client, identity).await?;
    let mut url = Url::parse("https://sub.wyzie.io/search")?;
    url.query_pairs_mut().extend_pairs([
        ("id", imdb.as_str()),
        ("key", key),
        ("language", language),
        ("format", "srt"),
        ("source", "all"),
        ("video", &identity.release),
    ]);
    if let Some((s, e)) = identity.episode {
        url.query_pairs_mut()
            .extend_pairs([("season", s.to_string()), ("episode", e.to_string())]);
    }
    let data = json(client, url).await?;
    let rows = data
        .as_array()
        .or_else(|| data["data"].as_array())
        .context("Respuesta de Wyzie inválida")?;
    let subtitles = rows
        .iter()
        .filter_map(|row| {
            let mut url = Url::parse(string(row, "url")).ok()?;
            if url.host_str() != Some("sub.wyzie.io") || !url.path().starts_with("/c/") {
                return None;
            }
            if !language_matches(string(row, "language"), language) {
                return None;
            }
            url.query_pairs_mut().append_pair("to", "srt");
            Some(OnlineSubtitle {
                id: format!("wyzie:{}:{}", string(row, "source"), string(row, "id")),
                provider: format!("Wyzie · {}", string(row, "source")),
                name: string(row, "release").to_owned(),
                language: language.into(),
                url: url.into(),
                format: "srt".into(),
            })
        })
        .take(40)
        .collect();
    Ok(SearchResult {
        title: identity.release.clone(),
        subtitles,
    })
}

pub async fn test_key(provider: &str, key: &str) -> Result<()> {
    let mut url = Url::parse(match provider {
        "subdl" => "https://api.subdl.com/api/v1/me",
        "wyzie" => "https://sub.wyzie.io/search?id=tt1254207&language=en&format=srt",
        _ => bail!("Proveedor desconocido"),
    })?;
    url.query_pairs_mut().append_pair(
        if provider == "subdl" {
            "api_key"
        } else {
            "key"
        },
        key,
    );
    let data = json(&client()?, url).await?;
    ensure!(
        data.get("error").is_none_or(|e| e.is_null() || e == false) && data["status"] != false,
        "El proveedor rechazó la clave. Comprobá tu cuenta y el cupo disponible"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn release_identity_preserves_movies_and_episodes() {
        let movie =
            identify("/private/videos/Night.of.the.Living.Dead.1968.1080p.BluRay.mkv").unwrap();
        assert_eq!(movie.title, "Night of the Living Dead");
        assert_eq!(movie.year.as_deref(), Some("1968"));
        assert!(!movie.release.contains("private"));
        let episode = identify("Show.Name.S02E03.720p.mkv").unwrap();
        assert_eq!(episode.title, "Show Name");
        assert_eq!(episode.episode, Some((2, 3)));
        assert_eq!(identify("Show 1x12.mp4").unwrap().episode, Some((1, 12)));
        assert_eq!(identify("1917.2019.mp4").unwrap().title, "1917");
        assert_eq!(
            identify("tt0944947 S01E01").unwrap().imdb.as_deref(),
            Some("tt0944947")
        );
    }
    #[test]
    fn blocks_untrusted_download_destinations() {
        for url in [
            "http://subs5.strem.io/a",
            "https://127.0.0.1/a",
            "https://subs5.strem.io.evil.org/a",
            "https://user:pass@subs5.strem.io/a",
            "https://api.thesubtitledb.org:444/a",
        ] {
            assert!(!allowed_url(&Url::parse(url).unwrap()));
        }
        assert!(allowed_url(
            &Url::parse("https://subs5.strem.io/en/download/file/123").unwrap()
        ));
        assert!(language_matches("ea", "es"));
        assert!(!language_matches("eng", "es"));
    }
    #[tokio::test]
    #[ignore = "Contacts two public subtitle services; run explicitly"]
    async fn live_public_providers() {
        for provider in ["opensubtitles", "subtitledb"] {
            let result = search(
                provider,
                "Night.of.the.Living.Dead.1968.1080p.mp4",
                "es",
                None,
            )
            .await
            .unwrap();
            assert!(!result.subtitles.is_empty(), "{provider}: no results");
            let first = &result.subtitles[0];
            let track = download(&first.url, &first.name).await.unwrap();
            assert!(!track.cues.is_empty(), "{provider}: empty track");
            eprintln!(
                "{provider}: {} results, {} cues",
                result.subtitles.len(),
                track.cues.len()
            );
        }
    }
}
