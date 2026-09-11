mod common;

use common::{MockEventEmitter, TestFixture};

#[tokio::test]
async fn e2e_large_file_integrity() {
    let pair = common::spawn_transfer_pair().await;
    let fixture = TestFixture::new();
    let source = fixture.create_large_file("large.bin", 10_000_000);
    let recv_dir = fixture.output_dir();

    let share = pair.share(vec![source.clone()], None)
        .await
        .expect("share should succeed");

    assert_eq!(share.size, 10_000_000);

    let (_cancel_tx, cancel_rx) = common::no_cancel();
    pair.download(&share.ticket, recv_dir.clone(), None, cancel_rx)
    .await
    .expect("download should succeed");

    let original = std::fs::read(&source).unwrap();
    let received = std::fs::read(recv_dir.join("large.bin")).expect("received file should exist");

    assert_eq!(
        received.len(),
        original.len(),
        "file size should match (10MB)"
    );
    assert_eq!(
        received, original,
        "10MB file should be identical after P2P transfer"
    );

    drop(share);
}

#[tokio::test]
async fn e2e_progress_events_emitted() {
    let pair = common::spawn_transfer_pair().await;
    let fixture = TestFixture::new();
    let source = fixture.create_large_file("progress.bin", 5_000_000);
    let recv_dir = fixture.output_dir();

    let receiver_emitter = MockEventEmitter::new();

    let share = pair.share(vec![source], None)
        .await
        .expect("share should succeed");

    let (_cancel_tx, cancel_rx) = common::no_cancel();
    pair.download(&share.ticket, recv_dir.clone(), Some(receiver_emitter.clone()), cancel_rx)
    .await
    .expect("download should succeed");

    let progress_events = receiver_emitter.events_with_name("receive-progress");
    assert!(
        !progress_events.is_empty(),
        "should have at least one progress event for a 5MB transfer"
    );

    for event in &progress_events {
        let payload = event
            .payload
            .as_ref()
            .expect("progress event should have payload");
        let parts: Vec<&str> = payload.split(':').collect();
        assert_eq!(
            parts.len(),
            3,
            "progress payload must be '<bytes_transferred>:<total_bytes>:<speed>', got: {}",
            payload
        );
    }

    assert!(receiver_emitter.has_event("receive-completed"));

    drop(share);
}
