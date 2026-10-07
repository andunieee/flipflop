mod common;

use common::TestFixture;

#[tokio::test]
async fn e2e_directory_roundtrip() {
    let pair = common::spawn_transfer_pair().await;
    let fixture = TestFixture::new();
    let source_dir = fixture.create_dir_with_files(
        "my_folder",
        &[
            ("root.txt", b"root file content"),
            ("sub/nested.txt", b"nested file content"),
        ],
    );
    let recv_dir = fixture.output_dir();

    let share = pair.share(vec![source_dir], None)
        .await
        .expect("share should succeed");

    assert_eq!(share.entry_type, "directory");

    let (_cancel_tx, cancel_rx) = common::no_cancel();
    pair.download(&share.ticket, recv_dir.clone(), None, cancel_rx)
    .await
    .expect("download should succeed");

    let root_content =
        std::fs::read(recv_dir.join("my_folder/root.txt")).expect("root.txt should exist");
    assert_eq!(root_content, b"root file content");

    let nested_content = std::fs::read(recv_dir.join("my_folder/sub/nested.txt"))
        .expect("sub/nested.txt should exist");
    assert_eq!(nested_content, b"nested file content");

    drop(share);
}

#[tokio::test]
async fn e2e_directory_with_deep_nesting() {
    let pair = common::spawn_transfer_pair().await;
    let fixture = TestFixture::new();
    let source_dir = fixture.create_dir_with_files(
        "deep",
        &[
            ("a.txt", b"level 0"),
            ("l1/b.txt", b"level 1"),
            ("l1/l2/c.txt", b"level 2"),
            ("l1/l2/l3/d.txt", b"level 3"),
        ],
    );
    let recv_dir = fixture.output_dir();

    let share = pair.share(vec![source_dir], None)
        .await
        .expect("share should succeed");

    let (_cancel_tx, cancel_rx) = common::no_cancel();
    pair.download(&share.ticket, recv_dir.clone(), None, cancel_rx)
    .await
    .expect("download should succeed");

    assert_eq!(
        std::fs::read(recv_dir.join("deep/a.txt")).unwrap(),
        b"level 0"
    );
    assert_eq!(
        std::fs::read(recv_dir.join("deep/l1/b.txt")).unwrap(),
        b"level 1"
    );
    assert_eq!(
        std::fs::read(recv_dir.join("deep/l1/l2/c.txt")).unwrap(),
        b"level 2"
    );
    assert_eq!(
        std::fs::read(recv_dir.join("deep/l1/l2/l3/d.txt")).unwrap(),
        b"level 3"
    );

    drop(share);
}
