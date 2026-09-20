//! Quick share E2E: a one-off share with a device we don't pair with.
//!
//! These bind real endpoints, so run with `--test-threads=1`.

mod common;

use common::{TestFixture, TestNode};
use flipflop::engine::Discoverability;
use std::time::Duration;

const EVENT_DEADLINE: Duration = Duration::from_secs(15);

/// Two unpaired nodes, the receiver taking unpaired control messages.
async fn strangers() -> (TestNode, TestNode) {
    let alice = common::spawn_node("alice").await;
    let bob = common::spawn_node("bob").await;
    bob.set_discoverability(Discoverability::Everyone).await;
    (alice, bob)
}

fn payload(node: &TestNode, event: &str) -> serde_json::Value {
    let events = node.events.events_with_name(event);
    assert_eq!(events.len(), 1, "exactly one {event}");
    serde_json::from_str(events[0].payload.as_deref().expect("payload")).expect("json payload")
}

#[tokio::test]
async fn an_accepted_quick_share_downloads_without_pairing() {
    let (alice, bob) = strangers().await;
    let fixture = TestFixture::new();
    let source = fixture.create_file("boarding-pass.pdf", b"seat 12A");
    let share = alice
        .share_with_peer(&bob.endpoint_id(), vec![source], None)
        .await
        .expect("share");

    let delivered = alice
        .quick_share(&bob.endpoint_id(), "send-1", &share.ticket, 1, 8, "boarding-pass.pdf")
        .await
        .expect("quick share delivery");
    assert!(delivered, "the quick share must reach bob");

    common::wait_until("bob to be offered the share", EVENT_DEADLINE, || {
        bob.events.has_event("quick-share-received")
    })
    .await;
    let offer = payload(&bob, "quick-share-received");
    assert_eq!(offer["id"], "send-1");
    assert_eq!(offer["title"], "boarding-pass.pdf");
    assert_eq!(offer["sender_name"], "alice");
    assert_eq!(offer["remote_endpoint_id"], alice.endpoint_id());
    assert!(
        !bob.events.has_event("paired-invite-received"),
        "a quick share is not an invite to auto-accept"
    );

    // Accepting is the download itself.
    let recv_dir = fixture.output_dir();
    let (_cancel_tx, cancel_rx) = common::no_cancel();
    bob.download_from_peer(
        &alice.endpoint_id(),
        offer["blob_ticket"].as_str().unwrap(),
        recv_dir.clone(),
        None,
        cancel_rx,
    )
    .await
    .expect("download");
    assert_eq!(
        std::fs::read(recv_dir.join("boarding-pass.pdf")).expect("received file"),
        b"seat 12A"
    );

    assert!(alice.list_paired().unwrap().is_empty(), "alice must not pair");
    assert!(bob.list_paired().unwrap().is_empty(), "bob must not pair");
}

#[tokio::test]
async fn a_declined_quick_share_tells_the_sender_which_one() {
    let (alice, bob) = strangers().await;
    alice.set_discoverability(Discoverability::Everyone).await;

    let delivered = alice
        .quick_share(&bob.endpoint_id(), "send-7", "test-blob-ticket", 1, 4, "notes.txt")
        .await
        .expect("quick share delivery");
    assert!(delivered);
    common::wait_until("bob to be offered the share", EVENT_DEADLINE, || {
        bob.events.has_event("quick-share-received")
    })
    .await;

    let answered = bob
        .decline_quick_share(&alice.endpoint_id(), "send-7")
        .await
        .expect("decline delivery");
    assert!(answered);
    common::wait_until("alice to hear the decline", EVENT_DEADLINE, || {
        alice.events.has_event("quick-share-declined")
    })
    .await;
    let declined = payload(&alice, "quick-share-declined");
    assert_eq!(declined["id"], "send-7");
    assert_eq!(declined["remote_endpoint_id"], bob.endpoint_id());

    assert!(alice.list_paired().unwrap().is_empty());
    assert!(bob.list_paired().unwrap().is_empty());
}

#[tokio::test]
async fn a_receiver_that_is_not_discoverable_refuses_quick_shares() {
    let alice = common::spawn_node("alice").await;
    let bob = common::spawn_node("bob").await;
    bob.set_discoverability(Discoverability::PairedOnly).await;

    let delivered = alice
        .quick_share(&bob.endpoint_id(), "send-1", "test-blob-ticket", 1, 4, "notes.txt")
        .await;
    // Refused at the handshake (not delivered) or dropped unread.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        !bob.events.has_event("quick-share-received"),
        "only `Everyone` takes shares from strangers (delivered: {delivered:?})"
    );
}
