mod common;

use common::{MockEventEmitter, TestFixture};

#[tokio::test]
async fn e2e_receiver_event_sequence() {
    let pair = common::spawn_transfer_pair().await;
    let fixture = TestFixture::new();
    let source = fixture.create_large_file("sequence.bin", 2_000_000);
    let recv_dir = fixture.output_dir();

    let receiver_emitter = MockEventEmitter::new();

    let share = pair.share(vec![source], None)
        .await
        .expect("share should succeed");

    let (_cancel_tx, cancel_rx) = common::no_cancel();
    pair.download(&share.ticket, recv_dir.clone(), Some(receiver_emitter.clone()), cancel_rx)
    .await
    .expect("download should succeed");

    let names = receiver_emitter.event_names();

    let started_idx = names
        .iter()
        .position(|n| n == "receive-started")
        .expect("should have receive-started event");

    let completed_idx = names
        .iter()
        .rposition(|n| n == "receive-completed")
        .expect("should have receive-completed event");

    assert!(
        started_idx < completed_idx,
        "receive-started (idx {}) must come before receive-completed (idx {})",
        started_idx,
        completed_idx,
    );

    let file_names_idx = names
        .iter()
        .position(|n| n == "receive-file-names")
        .expect("should have receive-file-names event");

    assert!(
        file_names_idx < completed_idx,
        "receive-file-names (idx {}) must come before receive-completed (idx {})",
        file_names_idx,
        completed_idx,
    );

    drop(share);
}

#[tokio::test]
async fn e2e_sender_events_on_transfer() {
    let pair = common::spawn_transfer_pair().await;
    let fixture = TestFixture::new();
    let source = fixture.create_large_file("sender_events.bin", 2_000_000);
    let recv_dir = fixture.output_dir();

    let sender_emitter = MockEventEmitter::new();

    let share = pair.share(vec![source], Some(sender_emitter.clone()))
    .await
    .expect("share should succeed");

    let (_cancel_tx, cancel_rx) = common::no_cancel();
    pair.download(&share.ticket, recv_dir, None, cancel_rx)
    .await
    .expect("download should succeed");

    tokio::time::timeout(tokio::time::Duration::from_secs(5), async {
        loop {
            if sender_emitter.has_event("transfer-completed") {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("timed out waiting for transfer-completed event");

    drop(share);
}

#[tokio::test]
async fn e2e_invalid_ticket_errors() {
    let pair = common::spawn_transfer_pair().await;
    let fixture = TestFixture::new();
    let (_cancel_tx, cancel_rx) = common::no_cancel();
    let result = pair
        .download("not-a-valid-ticket-string", fixture.output_dir(), None, cancel_rx)
        .await;

    assert!(
        result.is_err(),
        "download with invalid ticket should return error"
    );
}
