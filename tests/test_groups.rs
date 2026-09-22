//! Groups of paired peers: an invitation travels as the group's document,
//! and members who are not paired with each other still sync, by dialing.
//!
//! These bind real endpoints, so run with `--test-threads=1`.

mod common;

use std::time::Duration;

use flipflop::native::group::{MemberState, NewFile};

const DEADLINE: Duration = Duration::from_secs(90);

fn state_of(node: &common::TestNode, group: &str, who: &str) -> Option<MemberState> {
    node.groups()
        .groups()
        .get(group)?
        .members()
        .into_iter()
        .find(|m| m.endpoint_id == who)
        .map(|m| m.state)
}

#[tokio::test]
async fn invitation_is_accepted_and_files_are_listed() {
    let (alice, bob) = common::spawn_paired_nodes().await;
    let (alice_id, bob_id) = (alice.endpoint_id(), bob.endpoint_id());

    let group = alice.groups().create_group(&bob_id).await.unwrap();
    common::wait_until("bob to be invited", DEADLINE, || {
        state_of(&bob, &group, &bob_id) == Some(MemberState::Pending)
    })
    .await;
    assert!(bob.events.has_event("groups-changed"));

    bob.groups().answer_invite(&group, true).await.unwrap();
    common::wait_until("alice to see bob join", DEADLINE, || {
        state_of(&alice, &group, &bob_id) == Some(MemberState::Active)
    })
    .await;

    let file = alice
        .groups()
        .groups()
        .get(&group)
        .unwrap()
        .add_file(&NewFile {
            name: "notes.txt".into(),
            size: 5,
            hash: "abc".into(),
            path: "/nowhere/notes.txt".into(),
        })
        .unwrap();
    alice.groups().changed(&group).await;
    common::wait_until("bob to see the file", DEADLINE, || {
        bob.groups().groups().get(&group).unwrap().files().len() == 1
    })
    .await;
    let seen = bob.groups().groups().get(&group).unwrap().files();
    assert_eq!((seen[0].id.as_str(), seen[0].owner.as_str()), (file.as_str(), alice_id.as_str()));
}

#[tokio::test]
async fn members_not_paired_with_each_other_still_sync() {
    let (alice, bob) = common::spawn_paired_nodes().await;
    let carol = common::spawn_node("carol").await;
    common::pair(&bob, &carol).await;
    let (alice_id, bob_id, carol_id) = (alice.endpoint_id(), bob.endpoint_id(), carol.endpoint_id());

    let group = alice.groups().create_group(&bob_id).await.unwrap();
    common::wait_until("bob to be invited", DEADLINE, || {
        state_of(&bob, &group, &bob_id) == Some(MemberState::Pending)
    })
    .await;
    bob.groups().answer_invite(&group, true).await.unwrap();

    // Bob brings in carol, whom alice has never paired with.
    bob.groups().invite(&group, &carol_id).await.unwrap();
    common::wait_until("carol to be invited", DEADLINE, || {
        state_of(&carol, &group, &carol_id) == Some(MemberState::Pending)
    })
    .await;
    carol.groups().answer_invite(&group, true).await.unwrap();

    common::wait_until("alice to see carol join", DEADLINE, || {
        state_of(&alice, &group, &carol_id) == Some(MemberState::Active)
    })
    .await;
    common::wait_until("carol to see alice", DEADLINE, || {
        state_of(&carol, &group, &alice_id) == Some(MemberState::Active)
    })
    .await;

    // A file listed by carol reaches alice, with bob out of the way.
    bob.shutdown().await.unwrap();
    carol
        .groups()
        .groups()
        .get(&group)
        .unwrap()
        .add_file(&NewFile {
            name: "from-carol".into(),
            size: 1,
            hash: "h".into(),
            path: "/nowhere/from-carol".into(),
        })
        .unwrap();
    carol.groups().changed(&group).await;
    common::wait_until("alice to see carol's file directly", DEADLINE, || {
        alice.groups().groups().get(&group).unwrap().files().len() == 1
    })
    .await;
}

#[tokio::test]
async fn strangers_cannot_start_a_group_with_us() {
    let alice = common::spawn_node("alice").await;
    let bob = common::spawn_node("bob").await;
    assert!(bob.groups().create_group(&alice.endpoint_id()).await.is_err());
}

/// Downloads a group file the way the app does: ask a holder, fetch what it
/// offers, and say we hold it too.
async fn fetch(node: &common::TestNode, group: &str, file: &str, into: &std::path::Path) -> std::path::PathBuf {
    let (holder, ticket) = node.groups().request_file(group, file).await.unwrap();
    let (_cancel, cancel_rx) = tokio::sync::oneshot::channel();
    node.download_from_peer(&holder, &ticket, into.to_path_buf(), None, cancel_rx)
        .await
        .unwrap();
    let name = node
        .groups()
        .groups()
        .get(group)
        .unwrap()
        .files()
        .into_iter()
        .find(|f| f.id == file)
        .unwrap()
        .name;
    let path = into.join(name);
    node.groups().record_downloaded(group, file, &path).await.unwrap();
    path
}

#[tokio::test]
async fn files_are_fetched_from_holders_and_then_served_by_them() {
    let (alice, bob) = common::spawn_paired_nodes().await;
    let carol = common::spawn_node("carol").await;
    common::pair(&bob, &carol).await;
    let (bob_id, carol_id) = (bob.endpoint_id(), carol.endpoint_id());

    let group = alice.groups().create_group(&bob_id).await.unwrap();
    common::wait_until("bob to be invited", DEADLINE, || {
        state_of(&bob, &group, &bob_id) == Some(MemberState::Pending)
    })
    .await;
    bob.groups().answer_invite(&group, true).await.unwrap();
    bob.groups().invite(&group, &carol_id).await.unwrap();
    common::wait_until("carol to be invited", DEADLINE, || {
        state_of(&carol, &group, &carol_id) == Some(MemberState::Pending)
    })
    .await;
    carol.groups().answer_invite(&group, true).await.unwrap();

    // Alice lists a file; it is on her disk only.
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("report.txt");
    std::fs::write(&source, b"quarterly numbers").unwrap();
    alice.groups().share_file(&group, source).await.unwrap();
    common::wait_until("bob and carol to see the file", DEADLINE, || {
        [&bob, &carol]
            .iter()
            .all(|n| n.groups().groups().get(&group).unwrap().files().len() == 1)
    })
    .await;
    let file = bob.groups().groups().get(&group).unwrap().files()[0].id.clone();
    assert!(bob.groups().groups().get(&group).unwrap().files()[0].local_path.is_none());

    // Bob fetches it from alice, the only holder.
    let got = fetch(&bob, &group, &file, &dir.path().join("bob")).await;
    assert_eq!(std::fs::read(&got).unwrap(), b"quarterly numbers");
    common::wait_until("alice to see bob hold it", DEADLINE, || {
        alice.groups().groups().get(&group).unwrap().files()[0].holders.len() == 2
    })
    .await;

    // With alice gone, carol gets it from bob.
    common::wait_until("carol to see bob hold it", DEADLINE, || {
        carol.groups().groups().get(&group).unwrap().files()[0].holders.len() == 2
    })
    .await;
    alice.shutdown().await.unwrap();
    let got = fetch(&carol, &group, &file, &dir.path().join("carol")).await;
    assert_eq!(std::fs::read(&got).unwrap(), b"quarterly numbers");
}

#[tokio::test]
async fn nobody_online_holding_a_file_is_an_error_not_a_hang() {
    let (alice, bob) = common::spawn_paired_nodes().await;
    let bob_id = bob.endpoint_id();
    let group = alice.groups().create_group(&bob_id).await.unwrap();
    common::wait_until("bob to be invited", DEADLINE, || {
        state_of(&bob, &group, &bob_id) == Some(MemberState::Pending)
    })
    .await;
    bob.groups().answer_invite(&group, true).await.unwrap();

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("x.txt");
    std::fs::write(&source, b"x").unwrap();
    alice.groups().share_file(&group, source).await.unwrap();
    common::wait_until("bob to see the file", DEADLINE, || {
        !bob.groups().groups().get(&group).unwrap().files().is_empty()
    })
    .await;
    let file = bob.groups().groups().get(&group).unwrap().files()[0].id.clone();

    // The owner removes it before bob asks: bob's request is refused.
    alice.groups().remove_file(&group, &file).await.unwrap();
    common::wait_until("bob to see it removed", DEADLINE, || {
        bob.groups().groups().get(&group).unwrap().files().is_empty()
    })
    .await;
    assert!(bob.groups().request_file(&group, &file).await.is_err());
}
