//! A node serves a share only to the peer it was made for, only while it is
//! open, and only from the node's own endpoint.

mod common;

use common::{MockEventEmitter, TestFixture};
use std::str::FromStr;
use std::time::Duration;

#[tokio::test]
async fn the_ticket_is_served_by_the_senders_own_node() {
    let pair = common::spawn_transfer_pair().await;
    let fixture = TestFixture::new();
    let source = fixture.create_file("id.txt", b"same id");

    let share = pair.share(vec![source], None).await.expect("share");

    let ticket = iroh_blobs::ticket::BlobTicket::from_str(&share.ticket).expect("ticket");
    assert_eq!(
        ticket.addr().id.to_string(),
        pair.sender.endpoint_id(),
        "no throwaway endpoint: the ticket names the sender's node"
    );
}

#[tokio::test]
async fn a_peer_the_share_was_not_made_for_is_refused() {
    let pair = common::spawn_transfer_pair().await;
    let stranger = common::spawn_transfer_pair().await.receiver;
    // Even paired with the sender, a peer can only fetch what was shared with it.
    pair.sender
        .remember_paired_device_for_tests(&stranger.endpoint_id())
        .await
        .expect("sender remembers stranger");
    let fixture = TestFixture::new();
    let source = fixture.create_file("private.txt", b"only for the receiver");

    let share = pair.share(vec![source], None).await.expect("share");

    let (_cancel_tx, cancel_rx) = common::no_cancel();
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        stranger.download_from_peer(
            &pair.sender.endpoint_id(),
            &share.ticket,
            fixture.output_dir(),
            None,
            cancel_rx,
        ),
    )
    .await
    .expect("refusal must not hang");
    assert!(result.is_err(), "a stranger must not get the files");
    assert!(!fixture.output_dir().join("private.txt").exists());
}

#[tokio::test]
async fn a_ticket_from_another_node_is_rejected() {
    let pair = common::spawn_transfer_pair().await;
    let fixture = TestFixture::new();
    let source = fixture.create_file("spoof.txt", b"spoofed");

    let share = pair.share(vec![source], None).await.expect("share");

    // The receiver believes the ticket came from itself, not the sender.
    let (_cancel_tx, cancel_rx) = common::no_cancel();
    let result = pair
        .receiver
        .download_from_peer(
            &pair.receiver.endpoint_id(),
            &share.ticket,
            fixture.output_dir(),
            None,
            cancel_rx,
        )
        .await;
    assert!(result.is_err(), "the ticket's node must be the inviting peer");
}

#[tokio::test]
async fn closing_a_share_aborts_a_running_transfer() {
    let pair = common::spawn_transfer_pair().await;
    let fixture = TestFixture::new();
    let source = fixture.create_large_file("big.bin", 256 * 1024 * 1024);

    let share = pair.share(vec![source], None).await.expect("share");
    let ticket = share.ticket.clone();

    let emitter = MockEventEmitter::new();
    let (_cancel_tx, cancel_rx) = common::no_cancel();
    let download = pair.download(&ticket, fixture.output_dir(), Some(emitter.clone()), cancel_rx);
    tokio::pin!(download);

    // Let some bytes flow, then close the share mid-transfer.
    let started = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            tokio::select! {
                result = &mut download => return Err(result),
                _ = tokio::time::sleep(Duration::from_millis(5)) => {
                    if emitter.has_event("receive-progress") {
                        return Ok(());
                    }
                }
            }
        }
    })
    .await
    .expect("transfer never started");
    if let Err(result) = started {
        panic!("transfer finished before it could be interrupted: {:?}", result.is_ok());
    }
    drop(share);

    let result = tokio::time::timeout(Duration::from_secs(30), download)
        .await
        .expect("an aborted transfer must not hang");
    assert!(result.is_err(), "closing the share must stop the transfer");
}
