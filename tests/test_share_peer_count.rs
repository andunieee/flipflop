mod common;

use common::TestFixture;

/// A share counts every full pull of its payload, so a history row can tell
/// a peer that fetched twice apart from one that never finished.
#[tokio::test]
async fn share_counts_every_completed_full_pull() {
    let pair = common::spawn_transfer_pair().await;
    let fixture = TestFixture::new();
    let file = fixture.create_file("broadcast.bin", &vec![0xCD; 4096]);

    let share = pair.share(vec![file], None)
        .await
        .expect("share should succeed");

    assert_eq!(share.completed_peers(), 0, "nobody has pulled yet");

    for _ in 0..2 {
        let recv_dir = fixture.output_dir();
        let (_cancel_tx, cancel_rx) = common::no_cancel();
        pair.download(&share.ticket, recv_dir, None, cancel_rx)
        .await
        .expect("download should succeed");
    }

    // The sender tallies a pull once its request winds down, which can trail
    // the receiver's return by a moment.
    common::wait_until("both pulls to be counted", std::time::Duration::from_secs(10), || {
        share.completed_peers() == 2
    })
    .await;

    drop(share);
}
