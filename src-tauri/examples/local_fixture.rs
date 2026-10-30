//! Local-only fixture for exercising the real desktop torrent/magnet flow.
//!
//! From the repository root:
//! sh scripts/with-rust.sh cargo run --manifest-path src-tauri/Cargo.toml \
//!   --example local_fixture -- .local-test/test.mp4 .local-test/test.torrent
//!
//! Keep this process running while opening the generated .torrent or printed
//! magnet in Mukure. Ctrl-C stops both local listeners; the source stays intact.

use std::{
    env,
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    time::Duration,
};

use anyhow::{ensure, Context, Result};
use axum::{body::Bytes, extract::State, http::header, routing::get, Router};
use librqbit::{
    create_torrent, spawn_utils::BlockingSpawner, AddTorrent, AddTorrentOptions,
    CreateTorrentOptions, ListenerMode, ListenerOptions, Session, SessionOptions,
};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// BEP 3 compact peer format: one IPv4 address (4 bytes) and TCP port (2 bytes).
fn tracker_response(peer: SocketAddr) -> Bytes {
    let mut response =
        b"d8:completei1e10:incompletei0e8:intervali5e12:min intervali1e5:peers6:".to_vec();
    response.extend_from_slice(&Ipv4Addr::LOCALHOST.octets());
    response.extend_from_slice(&peer.port().to_be_bytes());
    response.push(b'e');
    response.into()
}

async fn announce(State(body): State<Bytes>) -> ([(header::HeaderName, &'static str); 2], Bytes) {
    (
        [
            (header::CONTENT_TYPE, "application/x-bittorrent"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = env::args_os().collect();
    ensure!(
        args.len() == 3,
        "Usage: local_fixture INPUT_VIDEO OUTPUT_TORRENT"
    );
    let input = tokio::fs::canonicalize(PathBuf::from(&args[1]))
        .await
        .context("input video does not exist")?;
    ensure!(input.is_file(), "input must be a video file");
    let output = PathBuf::from(&args[2]);
    let seed_folder = input.parent().context("input has no parent folder")?;
    let cancel = CancellationToken::new();
    // Any early error also stops network activity during unwinding.
    let _cancel_on_drop = cancel.clone().drop_guard();
    let tracker = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let tracker_url = format!("http://{}/announce", tracker.local_addr()?);
    let session = Session::new_with_opts(
        seed_folder.to_owned(),
        SessionOptions {
            dht: None,
            persistence: None,
            disable_trackers: true,
            disable_local_service_discovery: true,
            ipv4_only: true,
            cancellation_token: Some(cancel.child_token()),
            listen: Some(ListenerOptions {
                mode: ListenerMode::TcpOnly,
                listen_addr: (Ipv4Addr::LOCALHOST, 0).into(),
                enable_upnp_port_forwarding: false,
                ipv4_only: true,
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await
    .context("could not start loopback BitTorrent seeder")?;
    let peer = session
        .listen_addr()
        .context("seeder has no TCP listener")?;
    ensure!(peer.ip().is_loopback() && peer.port() != 0);
    let router = Router::new()
        .route("/announce", get(announce))
        .with_state(tracker_response(peer));
    let tracker_cancel = cancel.child_token();
    let mut tracker_task = tokio::spawn(async move {
        axum::serve(tracker, router)
            .with_graceful_shutdown(tracker_cancel.cancelled_owned())
            .await
    });

    let result = async {
        let torrent = create_torrent(
            &input,
            CreateTorrentOptions {
                trackers: vec![tracker_url.clone()],
                piece_length: Some(64 * 1024),
                ..Default::default()
            },
            &BlockingSpawner::new(2),
        )
        .await?;
        let bytes = torrent.as_bytes()?;
        let seed = session
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
            .context("torrent has no seed handle")?;
        tokio::time::timeout(Duration::from_secs(30), async {
            seed.wait_until_initialized().await?;
            seed.wait_until_completed().await
        })
        .await
        .context("seeder initialization exceeded 30 seconds")??;
        if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&output, bytes).await?;
        println!("Fixture ready; content is served only from this computer.");
        println!("VIDEO={}", input.display());
        println!(
            "TORRENT={}",
            tokio::fs::canonicalize(&output).await?.display()
        );
        println!("SEEDER={peer}");
        println!("TRACKER={tracker_url}");
        println!("MAGNET={}", torrent.as_magnet());
        println!("Open the torrent/magnet in Mukure. Press Ctrl-C here to stop.");
        tokio::signal::ctrl_c()
            .await
            .context("could not wait for Ctrl-C")?;
        Ok::<_, anyhow::Error>(())
    }
    .await;

    cancel.cancel();
    session.stop().await;
    match tokio::time::timeout(Duration::from_secs(3), &mut tracker_task).await {
        Ok(joined) => joined
            .context("tracker task failed")?
            .context("tracker failed")?,
        Err(_) => {
            tracker_task.abort();
            let _ = tracker_task.await;
        }
    }
    println!("Fixture stopped; original video preserved.");
    result
}
