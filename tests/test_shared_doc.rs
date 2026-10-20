//! The document shared with a paired peer: edits made on either side reach
//! the other over the paired connection and merge.
//!
//! These bind real endpoints, so run with `--test-threads=1`.

mod common;

use std::time::Duration;

const DEADLINE: Duration = Duration::from_secs(60);

#[tokio::test]
async fn edits_sync_both_ways_and_merge() {
    let (alice, bob) = common::spawn_paired_nodes().await;
    let (alice_id, bob_id) = (alice.endpoint_id(), bob.endpoint_id());

    // Edits made before the two are connected arrive once they are.
    alice.edit_shared_doc(&bob_id, "hello").await.unwrap();
    bob.edit_shared_doc(&alice_id, "world").await.unwrap();

    common::wait_until("both documents to hold both edits", DEADLINE, || {
        let a = alice.shared_doc_text(&bob_id).unwrap();
        a.contains("hello") && a.contains("world") && a == bob.shared_doc_text(&alice_id).unwrap()
    })
    .await;
    assert!(bob.events.has_event("shared-doc-changed"));
    assert!(alice.events.has_event("shared-doc-changed"));

    // Later edits flow as they are made.
    let text = alice.shared_doc_text(&bob_id).unwrap();
    alice
        .edit_shared_doc(&bob_id, &format!("{text} and more"))
        .await
        .unwrap();
    common::wait_until("bob to see alice's edit", DEADLINE, || {
        bob.shared_doc_text(&alice_id).unwrap() == format!("{text} and more")
    })
    .await;
    bob.edit_shared_doc(&alice_id, "replaced").await.unwrap();
    common::wait_until("alice to see bob's edit", DEADLINE, || {
        alice.shared_doc_text(&bob_id).unwrap() == "replaced"
    })
    .await;
}

#[tokio::test]
async fn only_paired_peers_have_a_document() {
    let alice = common::spawn_node("alice").await;
    let bob = common::spawn_node("bob").await;
    assert!(alice.edit_shared_doc(&bob.endpoint_id(), "hi").await.is_err());
}
