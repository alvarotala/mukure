//! Real BitTorrent transfers confined to loopback. No public torrent, tracker,
//! DHT bootstrap, multicast discovery, external executable, or mock is involved.

use std::{
    io::SeekFrom,
    net::{Ipv4Addr, SocketAddr},
    path::Path,
    sync::Arc,
    time::Duration,
};

use anyhow::{ensure, Context, Result};
use librqbit::{
    api::Api, create_torrent, spawn_utils::BlockingSpawner, AddTorrent, AddTorrentOptions,
    CreateTorrentOptions, ListenerMode, ListenerOptions, Session, SessionOptions,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::sync::CancellationToken;

// Also cancel background peer tasks when a timeout or assertion drops the test
// future before the explicit, orderly Session::stop calls can run.
struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

fn offline_options(cancel: &CancellationToken, listen: bool) -> SessionOptions {
    SessionOptions {
        dht: None,
        persistence: None,
        disable_trackers: true,
        disable_local_service_discovery: true,
        ipv4_only: true,
        cancellation_token: Some(cancel.child_token()),
        listen: listen.then(|| ListenerOptions {
            mode: ListenerMode::TcpOnly,
            listen_addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            enable_upnp_port_forwarding: false,
            ipv4_only: true,
            ..Default::default()
        }),
        ..Default::default()
    }
}

async fn transfer_and_seek(
    seeder: &Arc<Session>,
    leecher: &Arc<Session>,
    seed_path: &Path,
    cache_path: &Path,
    expected: &[u8],
    magnet: bool,
) -> Result<()> {
    let torrent = create_torrent(
        seed_path,
        CreateTorrentOptions {
            piece_length: Some(16 * 1024),
            trackers: Vec::new(),
            ..Default::default()
        },
        &BlockingSpawner::new(2),
    )
    .await
    .context("create synthetic test torrent")?;
    let bytes = torrent.as_bytes()?;
    let seed = seeder
        .add_torrent(
            AddTorrent::from_bytes(bytes.clone()),
            Some(AddTorrentOptions {
                overwrite: true,
                disable_trackers: true,
                ..Default::default()
            }),
        )
        .await?
        .into_handle()
        .context("seed handle")?;
    seed.wait_until_initialized().await?;
    seed.wait_until_completed().await?;
    let address = seeder.listen_addr().context("loopback seeder address")?;
    ensure!(address.ip().is_loopback(), "seeder must stay on loopback");
    ensure!(address.port() != 0, "seeder must have a bound TCP port");

    // librqbit's magnet parser does not implement x.pe. Its supported
    // initial_peers option discovers exactly this peer for BEP 9 metadata.
    let source = if magnet {
        AddTorrent::from_url(torrent.as_magnet().to_string())
    } else {
        AddTorrent::from_bytes(bytes)
    };
    let download = leecher
        .add_torrent(
            source,
            Some(AddTorrentOptions {
                paused: true,
                disable_trackers: true,
                initial_peers: Some(vec![address]),
                // No background file selection: only the active stream may
                // request pieces. This makes the cursor-priority check real.
                only_files: Some(Vec::new()),
                ..Default::default()
            }),
        )
        .await
        .context("add leecher torrent / resolve magnet metadata locally")?
        .into_handle()
        .context("leecher handle")?;
    download.wait_until_initialized().await?;
    let id = download.id();
    let api = Api::new(leecher.clone(), None);
    let details = api.api_torrent_details(id.into())?;
    ensure!(details.info_hash == torrent.info_hash().as_string());
    let files = details.files.context("resolved torrent file list")?;
    ensure!(files.len() == 1);
    ensure!(files[0].name == "test.mp4");
    ensure!(files[0].length == expected.len() as u64);
    ensure!(download.stats().file_progress == vec![0]);

    // Register and seek while paused, so the first requested pieces must be
    // near EOF, rather than beginning a sequential/full-file download.
    let mut reader = download.clone().stream(0).await?;
    ensure!(reader.len() == expected.len() as u64);
    let tail_start = expected.len() - 8192;
    reader.seek(SeekFrom::Start(tail_start as u64)).await?;
    leecher.unpause(&download).await?;
    let mut tail = vec![0; 8192];
    reader
        .read_exact(&mut tail)
        .await
        .context("stream missing tail pieces")?;
    ensure!(tail == expected[tail_start..]);
    ensure!(reader.position() == expected.len() as u64);

    let (haves, pieces) = api.api_dump_haves(id.into())?;
    ensure!(pieces > 2);
    ensure!(haves[pieces as usize - 1], "tail piece was not verified");
    ensure!(
        !haves[0],
        "cursor stream unexpectedly downloaded the first piece"
    );
    ensure!(
        haves.count_ones() < pieces as usize,
        "stream waited for the entire file"
    );

    // The same reader can seek backwards into still-missing pieces and return
    // the original bytes, exercising reprioritization rather than disk zeros.
    reader.seek(SeekFrom::Start(0)).await?;
    let mut head = vec![0; 4096];
    reader
        .read_exact(&mut head)
        .await
        .context("seek back to missing first piece")?;
    ensure!(head == expected[..head.len()]);
    let middle_start = expected.len() / 2 + 73;
    reader.seek(SeekFrom::Start(middle_start as u64)).await?;
    let mut middle = vec![0; 4096];
    reader
        .read_exact(&mut middle)
        .await
        .context("seek to middle pieces")?;
    ensure!(middle == expected[middle_start..middle_start + middle.len()]);
    reader.seek(SeekFrom::End(0)).await?;
    ensure!(reader.read(&mut [0; 1]).await? == 0);
    drop(reader);

    let downloaded_path = cache_path.join("test.mp4");
    ensure!(
        downloaded_path.is_file(),
        "torrent did not write into its cache"
    );
    leecher.delete(id.into(), true).await?;
    ensure!(
        leecher.get(id.into()).is_none(),
        "deleted torrent remains managed"
    );
    ensure!(
        !downloaded_path.exists(),
        "delete(true) left downloaded data"
    );
    ensure!(
        tokio::fs::read(seed_path).await? == expected,
        "leecher cleanup changed the seed"
    );
    Ok(())
}

async fn local_transfer(magnet: bool) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(30), async {
        let root = tempfile::tempdir()?;
        let seed_dir = root.path().join("seed");
        tokio::fs::create_dir_all(&seed_dir).await?;
        let cache = tempfile::Builder::new()
            .prefix("session-")
            .tempdir_in(root.path())?;
        let cache_path = cache.path().to_path_buf();
        let seed_path = seed_dir.join("test.mp4");
        // An uneven length exercises a final partial BitTorrent piece. The
        // filename represents media, but its deterministic bytes need no codec.
        let expected: Vec<u8> = (0..1024 * 1024 + 317usize)
            .map(|index| ((index.wrapping_mul(73) ^ (index >> 8) ^ (index >> 16)) & 255) as u8)
            .collect();
        tokio::fs::write(&seed_path, &expected).await?;
        let cancel = CancelOnDrop(CancellationToken::new());
        let seeder = Session::new_with_opts(seed_dir, offline_options(&cancel.0, true)).await?;
        let leecher =
            Session::new_with_opts(cache_path.clone(), offline_options(&cancel.0, false)).await?;
        let result = transfer_and_seek(
            &seeder,
            &leecher,
            &seed_path,
            &cache_path,
            &expected,
            magnet,
        )
        .await;

        // Run shutdown even when a transfer assertion returns an error. Each
        // engine closes its tasks before its session cache is removed.
        tokio::join!(seeder.stop(), leecher.stop());
        drop(seeder);
        drop(leecher);
        cache.close()?;
        ensure!(
            !cache_path.exists(),
            "session cache directory survived cleanup"
        );
        result
    })
    .await
    .context("offline loopback BitTorrent test exceeded 30 seconds")?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn torrent_file_streams_seeked_pieces_and_deletes_cache() -> Result<()> {
    local_transfer(false).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn magnet_resolves_metadata_and_streams_from_loopback_peer() -> Result<()> {
    local_transfer(true).await
}
