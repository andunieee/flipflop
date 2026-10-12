//! A send queued for an offline peer: the app keeps the share open, lets
//! the invite fail while the peer is away, and delivers it once the peer is
//! back (transfers.rs `retry_waiting`). This checks the engine behaviour that
//! relies on: an unreachable peer is a clean `Ok(false)`, and the same share
//! still downloads after the peer restarts.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{wait_until, MockEventEmitter, TestFixture};
use flipflop::engine::identity_store::identity_key_path;
use flipflop::engine::{Discoverability, DiscoveryModeOption, NodeService, PairingStatus};
use iroh::endpoint::RelayMode;
use iroh::SecretKey;

const START_TIMEOUT: Duration = Duration::from_secs(60);

fn seed_identity(data_dir: &Path) -> String {
    std::fs::create_dir_all(data_dir).expect("create data dir");
    let secret = SecretKey::generate();
    std::fs::write(identity_key_path(data_dir), secret.to_bytes()).expect("write identity.key");
    data_encoding::HEXLOWER.encode(secret.public().as_bytes())
}

async fn start_node(data_dir: &Path, emitter: std::sync::Arc<MockEventEmitter>) -> NodeService {
    tokio::time::timeout(
        START_TIMEOUT,
        NodeService::start(
            data_dir,
            RelayMode::Default,
            DiscoveryModeOption::Default,
            Discoverability::default(),
            Some(emitter),
        ),
    )
    .await
    .expect("node start timed out")
    .expect("node start failed")
}

fn paired_status(node: &NodeService, endpoint_id: &str) -> Option<PairingStatus> {
    node.list_paired()
        .expect("list_paired")
        .into_iter()
        .find(|d| d.endpoint_id.eq_ignore_ascii_case(endpoint_id))
        .map(|d| d.pairing_status)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queued_send_reaches_peer_after_it_comes_back() {
    let fixture = TestFixture::new();
    let source = fixture.create_file("queued.txt", b"sent while you were away");
    let recv_dir = fixture.output_dir();

    let host_dir = tempfile::tempdir().expect("host dir");
    let joiner_dir = tempfile::tempdir().expect("joiner dir");
    let _host_id = seed_identity(host_dir.path());
    let joiner_id = seed_identity(joiner_dir.path());

    let host_events = MockEventEmitter::new();
    let joiner_events = MockEventEmitter::new();
    let (host, joiner) = tokio::join!(
        start_node(host_dir.path(), host_events.clone()),
        start_node(joiner_dir.path(), joiner_events.clone())
    );

    let ticket = host
        .start_pairing_host(Some(300))
        .await
        .expect("open pairing window");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        match tokio::time::timeout(Duration::from_secs(30), joiner.join_pairing(&ticket)).await {
            Ok(Ok(())) => break,
            Ok(Err(err)) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "join failed: {err:#}"
                );
            }
            Err(_) => assert!(tokio::time::Instant::now() < deadline, "join hung"),
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    host.stop_pairing_host().await;
    wait_until("host stores joiner", Duration::from_secs(30), || {
        paired_status(&host, &joiner_id) == Some(PairingStatus::Active)
    })
    .await;

    // The peer goes away.
    joiner.shutdown().await.expect("joiner shutdown");
    drop(joiner);

    // Send anyway: the share opens, the invite can't be delivered.
    let share = host
        .share_with_peer(&joiner_id, vec![source], None)
        .await
        .expect("share_with_peer");
    let delivered = tokio::time::timeout(
        Duration::from_secs(90),
        host.invite_paired_device(&joiner_id, &share.ticket, 1, share.size),
    )
    .await
    .expect("invite to an offline peer must give up, not hang")
    .expect("an unreachable peer is not an error");
    assert!(!delivered, "nobody is there to take the invite");

    // The peer comes back with the same identity; retry like the app does.
    let joiner_events = MockEventEmitter::new();
    let joiner = start_node(joiner_dir.path(), joiner_events.clone()).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        let delivered = host
            .invite_paired_device(&joiner_id, &share.ticket, 1, share.size)
            .await
            .unwrap_or(false);
        if delivered {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "queued invite never reached the returning peer"
        );
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    wait_until("peer sees the invite", Duration::from_secs(20), || {
        joiner_events.has_event("paired-invite-received")
    })
    .await;
    let invite = joiner_events
        .events_with_name("paired-invite-received")
        .into_iter()
        .last()
        .expect("invite event");
    let payload: serde_json::Value =
        serde_json::from_str(invite.payload.as_deref().expect("payload")).expect("json");
    let blob_ticket = payload["blob_ticket"]
        .as_str()
        .expect("blob_ticket")
        .to_string();
    let remote_id = payload["remote_endpoint_id"]
        .as_str()
        .expect("id")
        .to_string();
    joiner
        .respond_paired_invite(&remote_id, true)
        .await
        .expect("respond_paired_invite");

    let (_cancel_tx, cancel_rx) = common::no_cancel();
    tokio::time::timeout(
        Duration::from_secs(30),
        joiner.download_from_peer(&remote_id, &blob_ticket, recv_dir.clone(), None, cancel_rx),
    )
    .await
    .expect("download must not hang")
    .expect("download should succeed");
    let received = std::fs::read(recv_dir.join("queued.txt")).expect("received file");
    assert_eq!(received, b"sent while you were away");
    drop(share);
}
