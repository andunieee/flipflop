//! Nodes in one process, talking over loopback/LAN: connecting, chat, contacts and groups.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use flipflop::core::{Core, Event, GroupsView, MemberState, Peer};
use tokio::time::{sleep, timeout};

#[derive(Default)]
struct Seen {
    peers: Vec<Peer>,
    groups: GroupsView,
    messages: Vec<String>,
}

type Log = Arc<Mutex<Seen>>;

struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("flipflop-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        Dir(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn node_in(dir: &Dir, name: &str) -> (Core, Log) {
    let log: Log = Arc::default();
    let l = log.clone();
    let core = Core::start(dir.path(), false, move |e| match e {
        Event::Peers(p) => l.lock().unwrap().peers = p,
        Event::Groups(g) => l.lock().unwrap().groups = g,
        Event::Message { text, mine: false, .. } => l.lock().unwrap().messages.push(text),
        _ => {}
    })
    .await
    .unwrap();
    core.set_name(name);
    (core, log)
}

async fn node(name: &str) -> (Core, Log, Dir) {
    let dir = Dir::new(name);
    let (core, log) = node_in(&dir, name).await;
    (core, log, dir)
}

async fn until_for(secs: u64, what: &str, mut f: impl FnMut() -> bool) {
    timeout(Duration::from_secs(secs), async {
        while !f() {
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

async fn until(what: &str, f: impl FnMut() -> bool) {
    until_for(20, what, f).await
}

fn peer(log: &Log, core: &Core) -> Option<Peer> {
    log.lock().unwrap().peers.iter().find(|p| p.id == core.id()).cloned()
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_by_ticket_and_chat() {
    let (a, alog, _da) = node("alice").await;
    let (b, blog, _db) = node("bob").await;

    until("alice ticket", || !a.ticket().is_empty()).await;
    b.connect_ticket(&a.ticket()).await.expect("connect");

    until("bob sees alice's name", || blog.lock().unwrap().peers.iter().any(|p| p.connected && p.name == "alice")).await;
    until("alice sees bob's name", || alog.lock().unwrap().peers.iter().any(|p| p.connected && p.name == "bob")).await;

    b.send(a.id(), "hi alice".into()).await.unwrap();
    until("message", || alog.lock().unwrap().messages == ["hi alice"]).await;

    // rename propagates over the live connection
    a.set_name("alicia");
    until("rename", || blog.lock().unwrap().peers.iter().any(|p| p.name == "alicia")).await;

    a.shutdown().await;
    b.shutdown().await;
}

/// Peers on the same network show up via mDNS (with their names) and connect by id alone.
#[tokio::test(flavor = "multi_thread")]
async fn lan_discovery_then_connect_by_id() {
    let (a, alog, _da) = node("lan-alice").await;
    let (b, blog, _db) = node("lan-bob").await;

    until("bob sees alice on LAN", || blog.lock().unwrap().peers.iter().any(|p| p.lan && p.id == a.id() && p.name == "lan-alice")).await;
    until("alice sees bob on LAN", || alog.lock().unwrap().peers.iter().any(|p| p.lan && p.id == b.id() && p.name == "lan-bob")).await;

    b.connect(a.id()).await.expect("connect by id via mDNS addresses");
    b.send(a.id(), "over lan".into()).await.unwrap();
    until("message", || alog.lock().unwrap().messages == ["over lan"]).await;

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn contacts_are_saved_renamed_and_reconnect_until_forgotten() {
    let (a, alog, da) = node("c-alice").await;
    let (b, blog, db) = node("c-bob").await;
    until("alice ticket", || !a.ticket().is_empty()).await;
    b.connect_ticket(&a.ticket()).await.unwrap();

    // both sides saved the other, with names
    until("contacts saved", || {
        peer(&blog, &a).is_some_and(|p| p.saved && p.name == "c-alice") && peer(&alog, &b).is_some_and(|p| p.saved && p.name == "c-bob")
    })
    .await;
    let json = std::fs::read_to_string(da.path().join("state.json")).unwrap();
    assert!(json.contains(&b.id().to_string()) && json.contains("c-bob"), "{json}");

    // local rename keeps the peer's own name
    b.rename_contact(a.id(), "my friend");
    until("alias", || peer(&blog, &a).is_some_and(|p| p.alias.as_deref() == Some("my friend") && p.name == "c-alice")).await;
    assert!(std::fs::read_to_string(db.path().join("state.json")).unwrap().contains("my friend"));

    // dropping the connection: saved contacts come back by themselves
    b.disconnect(a.id());
    until("disconnect", || peer(&blog, &a).is_some_and(|p| !p.connected)).await;
    until_for(30, "automatic reconnect", || peer(&blog, &a).is_some_and(|p| p.connected) && peer(&alog, &b).is_some_and(|p| p.connected)).await;

    // Alice deletes Bob, who still has her saved: he keeps redialing and she accepts, but that
    // must not save him again.
    a.forget_contact(b.id());
    a.disconnect(b.id());
    until("alice sees the drop", || peer(&alog, &b).is_none_or(|p| !p.connected)).await;
    until_for(30, "bob redials", || peer(&alog, &b).is_some_and(|p| p.connected)).await;
    sleep(Duration::from_secs(1)).await;
    assert!(peer(&alog, &b).is_some_and(|p| !p.saved), "deleted contact was saved again by connecting");
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(da.path().join("state.json")).unwrap()).unwrap();
    let bob = b.id().to_string();
    assert!(!saved["contacts"].as_array().unwrap().iter().any(|c| c["id"] == bob.as_str()), "deleted contact still in the file");
    assert!(saved["forgotten"].as_array().unwrap().iter().any(|f| *f == bob.as_str()));

    // Asking to connect to them ourselves saves them again.
    a.connect(b.id()).await.unwrap();
    until("saved again", || peer(&alog, &b).is_some_and(|p| p.saved)).await;

    // Deleted on both sides: stays disconnected.
    a.forget_contact(b.id());
    b.forget_contact(a.id());
    a.disconnect(b.id());
    b.disconnect(a.id());
    until("disconnect", || peer(&blog, &a).is_none_or(|p| !p.connected)).await;
    sleep(Duration::from_secs(10)).await;
    assert!(peer(&blog, &a).is_none_or(|p| !p.connected && !p.saved), "forgotten contact reconnected");
    assert!(peer(&alog, &b).is_none_or(|p| !p.connected && !p.saved), "forgotten contact reconnected");

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn group_invites_accept_offline_resend_and_rejection() {
    let (a, alog, _da) = node("g-alice").await;
    let (b, blog, _db) = node("g-bob").await;
    let dc = Dir::new("g-carol");
    let (c, clog) = node_in(&dc, "g-carol").await;

    until("alice ticket", || !a.ticket().is_empty()).await;
    b.connect_ticket(&a.ticket()).await.unwrap();
    until("carol ticket", || !c.ticket().is_empty()).await;
    a.connect_ticket(&c.ticket()).await.unwrap();
    until("contacts", || peer(&alog, &b).is_some_and(|p| p.saved) && peer(&alog, &c).is_some_and(|p| p.saved)).await;

    // --- online invite, accepted ---
    let g1 = a.create_group();
    let haiku = flipflop_haiku(&alog, &g1);
    assert_eq!(haiku.matches(" / ").count(), 2, "{haiku}");
    a.add_to_group(&g1, b.id());
    until("bob sees invite", || blog.lock().unwrap().groups.invites.iter().any(|i| i.group == g1 && i.inviter == a.id() && i.haiku == haiku)).await;
    until("alice marked it sent", || member_state(&alog, &g1, &b) == Some(MemberState::Sent)).await;
    b.accept_invite(a.id(), &g1);
    until("alice sees bob accepted", || member_state(&alog, &g1, &b) == Some(MemberState::Accepted)).await;
    until("bob is in the group", || {
        let v = &blog.lock().unwrap().groups;
        v.invites.is_empty() && v.groups.iter().any(|g| g.id == g1 && g.haiku == haiku && g.members.iter().any(|m| m.id == a.id() && m.state == MemberState::Accepted))
    })
    .await;

    // --- carol is offline when added: remembered, delivered when she is back ---
    c.shutdown().await;
    until("carol offline", || peer(&alog, &c).is_some_and(|p| !p.connected)).await;
    let g2 = a.create_group();
    a.add_to_group(&g2, c.id());
    assert_eq!(member_state(&alog, &g2, &c), Some(MemberState::Pending));
    let (c, clog2) = node_in(&dc, "g-carol").await; // same data dir: same identity
    drop(clog);
    until_for(40, "carol gets the invite after reconnecting", || clog2.lock().unwrap().groups.invites.iter().any(|i| i.group == g2)).await;
    until("alice marked it sent", || member_state(&alog, &g2, &c) == Some(MemberState::Sent)).await;

    // --- rejection is remembered ---
    c.reject_invite(a.id(), &g2);
    until("alice sees decline", || member_state(&alog, &g2, &c) == Some(MemberState::Declined)).await;
    assert!(clog2.lock().unwrap().groups.invites.is_empty());
    a.add_to_group(&g2, c.id()); // ask again
    until("alice asks again", || member_state(&alog, &g2, &c) == Some(MemberState::Sent) || member_state(&alog, &g2, &c) == Some(MemberState::Declined)).await;
    until("and gets the silent no", || member_state(&alog, &g2, &c) == Some(MemberState::Declined)).await;
    sleep(Duration::from_secs(1)).await;
    assert!(clog2.lock().unwrap().groups.invites.is_empty(), "rejected invite was shown again");

    // other invites from the same peer are still shown
    let g3 = a.create_group();
    a.add_to_group(&g3, c.id());
    until("other group's invite shows", || clog2.lock().unwrap().groups.invites.iter().any(|i| i.group == g3)).await;

    // local aliases keep the haiku
    a.rename_group(&g1, "weekend plans");
    until("alias", || alog.lock().unwrap().groups.groups.iter().any(|g| g.id == g1 && g.alias.as_deref() == Some("weekend plans") && g.haiku == haiku)).await;
    a.delete_group(&g3);
    until("deleted", || !alog.lock().unwrap().groups.groups.iter().any(|g| g.id == g3)).await;

    let _ = blog;
    a.shutdown().await;
    b.shutdown().await;
    c.shutdown().await;
}

fn flipflop_haiku(log: &Log, group: &str) -> String {
    log.lock().unwrap().groups.groups.iter().find(|g| g.id == group).map(|g| g.haiku.clone()).expect("group listed")
}

fn member_state(log: &Log, group: &str, who: &Core) -> Option<MemberState> {
    log.lock().unwrap().groups.groups.iter().find(|g| g.id == group)?.members.iter().find(|m| m.id == who.id()).map(|m| m.state)
}
