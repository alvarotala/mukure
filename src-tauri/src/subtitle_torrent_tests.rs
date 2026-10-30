//! Exercise the production subtitle reader over real, loopback-only BitTorrent.

use std::{collections::HashSet, net::Ipv4Addr, time::Duration};

use anyhow::{ensure, Context, Result};
use librqbit::{
    api::Api, create_torrent, spawn_utils::BlockingSpawner, AddTorrent, AddTorrentOptions,
    CreateTorrentOptions, ListenerMode, ListenerOptions, Session, SessionOptions,
};
use tokio_util::sync::CancellationToken;

use crate::{ActiveTorrent, Engine};

fn offline_options(cancel: &CancellationToken, seed: bool) -> SessionOptions {
    SessionOptions {
        dht: None,
        persistence: None,
        disable_trackers: true,
        disable_local_service_discovery: true,
        ipv4_only: true,
        cancellation_token: Some(cancel.child_token()),
        listen: seed.then(|| ListenerOptions {
            mode: ListenerMode::TcpOnly,
            listen_addr: (Ipv4Addr::LOCALHOST, 0).into(),
            enable_upnp_port_forwarding: false,
            ipv4_only: true,
            ..Default::default()
        }),
        ..Default::default()
    }
}

async fn exercise_reader(with_peer: bool) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(20), async {
        let root = tempfile::tempdir()?;
        let seed_folder = root.path().join("seed");
        tokio::fs::create_dir(&seed_folder).await?;
        tokio::fs::write(seed_folder.join("README.txt"), b"not a video or subtitle").await?;
        let video_bytes: Vec<_> = (0..1024 * 1024usize)
            .map(|i| (i ^ (i >> 8)) as u8)
            .collect();
        let video_path = seed_folder.join("movie.mp4");
        tokio::fs::write(&video_path, &video_bytes).await?;
        // Multiple independent pieces ensure the whole excluded subtitle cannot
        // accidentally arrive as the tail of a video boundary piece.
        let caption = format!("¡Español! {} café", "é".repeat(4096));
        let subtitle = (0..16)
            .map(|i| {
                format!(
                    "{}\n00:00:{:02},250 --> 00:00:{:02},750\n{}\n\n",
                    i + 1,
                    i * 2,
                    i * 2 + 1,
                    caption
                )
            })
            .collect::<String>();
        tokio::fs::write(seed_folder.join("español.srt"), subtitle.as_bytes()).await?;
        let created = create_torrent(
            &seed_folder,
            CreateTorrentOptions {
                piece_length: Some(16 * 1024),
                ..Default::default()
            },
            &BlockingSpawner::new(2),
        )
        .await?;
        let torrent_bytes = created.as_bytes()?;
        let cancel = CancellationToken::new();
        let _cancel_on_drop = cancel.clone().drop_guard();
        let seeder =
            Session::new_with_opts(seed_folder.clone(), offline_options(&cancel, true)).await?;
        let seeded = seeder
            .add_torrent(
                AddTorrent::from_bytes(torrent_bytes.clone()),
                Some(AddTorrentOptions {
                    overwrite: true,
                    output_folder: Some(seed_folder.to_string_lossy().into_owned()),
                    ..Default::default()
                }),
            )
            .await?
            .into_handle()
            .context("seeder handle")?;
        seeded.wait_until_initialized().await?;
        seeded.wait_until_completed().await?;

        let engine = Engine::new(&root.path().join("cache")).await?;
        let cache = engine.cache.path().to_owned();
        let leecher =
            Session::new_with_opts(cache.clone(), offline_options(&cancel, false)).await?;
        engine
            .session
            .set(leecher.clone())
            .map_err(|_| anyhow::anyhow!("engine already has a session"))?;
        let handle = leecher
            .add_torrent(
                AddTorrent::from_bytes(torrent_bytes),
                Some(AddTorrentOptions {
                    paused: true,
                    only_files: Some(Vec::new()),
                    output_folder: Some(cache.to_string_lossy().into_owned()),
                    initial_peers: with_peer.then(|| vec![seeder.listen_addr().unwrap()]),
                    ..Default::default()
                }),
            )
            .await?
            .into_handle()
            .context("leecher handle")?;
        handle.wait_until_initialized().await?;
        let id = handle.id();
        // Use positions in the original metadata list, including README.txt.
        // The subtitle API must never reinterpret them as filtered-video IDs.
        let api = Api::new(leecher.clone(), None);
        let files = api
            .api_torrent_details(id.into())?
            .files
            .context("file list")?;
        ensure!(files.len() == 3);
        let video_id = files
            .iter()
            .position(|f| f.name == "movie.mp4")
            .context("video ID")?;
        let subtitle_id = files
            .iter()
            .position(|f| f.name == "español.srt")
            .context("subtitle ID")?;
        let readme_id = files
            .iter()
            .position(|f| f.name == "README.txt")
            .context("README ID")?;
        ensure!(video_id != subtitle_id && subtitle_id != readme_id);
        leecher
            .update_only_files(&handle, &HashSet::from([video_id]))
            .await?;
        let subtitle_cancel = CancellationToken::new();
        *engine.active.lock().await = Some(ActiveTorrent {
            handle: handle.clone(),
            selected: Some(video_id),
            subtitles_cancel: subtitle_cancel.clone(),
        });
        let result = async {
            ensure!(engine.read_torrent_subtitle(id, video_id).await.is_err());
            ensure!(engine.read_torrent_subtitle(id, readme_id).await.is_err());
            ensure!(engine
                .read_torrent_subtitle(id + 10, subtitle_id)
                .await
                .is_err());
            leecher.unpause(&handle).await?;
            if with_peer {
                let track = engine.read_torrent_subtitle(id, subtitle_id).await?;
                ensure!(track.name == "español.srt");
                ensure!(track.cues.len() == 16);
                ensure!(track.cues[0].start == 0.25 && track.cues[0].end == 1.75);
                ensure!(track.cues[15].text == caption);
                let details = api
                    .api_torrent_details(id.into())?
                    .files
                    .context("updated file list")?;
                ensure!(details[video_id].included);
                ensure!(
                    !details[subtitle_id].included,
                    "reading a sidecar changed background download selection"
                );
                ensure!(!details[readme_id].included);
            } else {
                let task_engine = engine.clone();
                let mut request = tokio::spawn(async move {
                    task_engine.read_torrent_subtitle(id, subtitle_id).await
                });
                // The subtitle has no peers: it must still be pending rather
                // than reading unverified zeros from its sparse cache file.
                ensure!(
                    tokio::time::timeout(Duration::from_millis(100), &mut request)
                        .await
                        .is_err()
                );
                {
                    let active =
                        tokio::time::timeout(Duration::from_millis(500), engine.active.lock())
                            .await
                            .context("subtitle download held the session lock")?;
                    ensure!(active.as_ref().map(|t| t.handle.id()) == Some(id));
                }
                subtitle_cancel.cancel();
                let error = tokio::time::timeout(Duration::from_secs(1), &mut request)
                    .await
                    .context("cancelled subtitle read did not finish promptly")??
                    .expect_err("cancelled subtitle read unexpectedly succeeded");
                ensure!(error.to_string().contains("selección de video cambió"));
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        tokio::join!(seeder.stop(), engine.shutdown());
        ensure!(!cache.exists(), "shutdown left downloaded subtitle cache");
        ensure!(
            tokio::fs::read(video_path).await? == video_bytes,
            "shutdown changed source video"
        );
        result
    })
    .await
    .context("subtitle integration test exceeded 20 seconds")?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reads_excluded_subtitle_from_real_peer_using_original_file_index() -> Result<()> {
    exercise_reader(true).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_peer_subtitle_read_releases_session_lock_and_cancels_promptly() -> Result<()> {
    exercise_reader(false).await
}
