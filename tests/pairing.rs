//! Pairing: two devices that both chose to pair share an implied group.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use flipflop::core::{Core, Event, GroupsView, Peer};
use tokio::time::{sleep, timeout};

#[derive(Default)]
struct Seen {
    peers: Vec<Peer>,
    groups: GroupsView,
}

type Log = Arc<Mutex<Seen>>;

struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("flipflop-pair-{tag}-{}", std::process::id()));
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

async fn node(name: &str) -> (Core, Log, Dir) {
    let dir = Dir::new(name);
    let log: Log = Arc::default();
    let l = log.clone();
    let core = Core::start(dir.path(), false, move |e| match e {
        Event::Peers(p) => l.lock().unwrap().peers = p,
        Event::Groups(g) => l.lock().unwrap().groups = g,
        _ => {}
    })
    .await
    .unwrap();
    core.set_name(name);
    (core, log, dir)
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    timeout(Duration::from_secs(20), async {
        while !f() {
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

fn peer(log: &Log, who: &Core) -> Option<Peer> {
    log.lock()
        .unwrap()
        .peers
        .iter()
        .find(|p| p.id == who.id())
        .cloned()
}

fn implied(log: &Log) -> Vec<String> {
    log.lock()
        .unwrap()
        .groups
        .groups
        .iter()
        .filter(|g| g.with.is_some())
        .map(|g| g.id.clone())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn implied_group_needs_both_sides_and_shares_content() {
    let (a, alog, _da) = node("p-alice").await;
    let (b, blog, _db) = node("p-bob").await;
    until("alice ticket", || !a.ticket().is_empty()).await;

    // Bob scans Alice's code: that pairs him with her, but she has not paired back.
    b.connect_ticket(&a.ticket()).await.unwrap();
    until("alice hears bob paired", || {
        peer(&alog, &b).is_some_and(|p| p.paired_us && !p.paired)
    })
    .await;
    assert!(peer(&blog, &a).unwrap().paired);
    assert!(
        implied(&alog).is_empty() && implied(&blog).is_empty(),
        "one-sided pairing is not enough"
    );

    // Alice pairs from the peers menu: now both have an implied group, with the same id.
    a.pair(b.id());
    until("implied groups", || {
        implied(&alog).len() == 1 && implied(&blog).len() == 1
    })
    .await;
    assert_eq!(implied(&alog), implied(&blog));
    let gid = implied(&alog).remove(0);
    assert_eq!(
        peer(&alog, &b).unwrap().pair_group.as_deref(),
        Some(gid.as_str())
    );
    assert_eq!(
        peer(&blog, &a).unwrap().pair_group.as_deref(),
        Some(gid.as_str())
    );

    // It carries content like any group.
    until("both in the doc", || {
        a.group_view(&gid).is_some_and(|v| v.members.len() == 2)
    })
    .await;
    a.send_text(&gid, "hello bob").unwrap();
    until("bob gets the text", || {
        b.group_view(&gid)
            .is_some_and(|v| v.texts.iter().any(|t| t.text == "hello bob"))
    })
    .await;

    // An implied group cannot be left on its own.
    a.delete_group(&gid);
    assert_eq!(implied(&alog).len(), 1);

    // Unpairing removes it on both sides.
    b.unpair(a.id());
    until("groups gone", || {
        implied(&alog).is_empty() && implied(&blog).is_empty()
    })
    .await;
    assert!(a.group_view(&gid).is_none());

    // Pairing again brings back a group with the same id.
    b.pair(a.id());
    until("back", || {
        implied(&alog) == [gid.clone()] && implied(&blog) == [gid.clone()]
    })
    .await;

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unpairing_while_apart_is_announced_on_reconnect() {
    let (a, alog, da) = node("u-alice").await;
    let (b, blog, _db) = node("u-bob").await;
    until("alice ticket", || !a.ticket().is_empty()).await;
    b.connect_ticket(&a.ticket()).await.unwrap();
    until("alice hears bob", || {
        peer(&alog, &b).is_some_and(|p| p.paired_us)
    })
    .await;
    a.pair(b.id());
    until("implied", || {
        implied(&alog).len() == 1 && implied(&blog).len() == 1
    })
    .await;

    // Alice goes away, restarts and has unpaired meanwhile (state edited while she was down).
    a.shutdown().await;
    until("bob sees alice gone", || {
        peer(&blog, &a).is_some_and(|p| !p.connected)
    })
    .await;
    drop(a);
    let state_path = da.path().join("state.json");
    let mut state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    for c in state["contacts"].as_array_mut().unwrap() {
        c["paired"] = false.into();
    }
    state["groups"] = serde_json::json!([]);
    std::fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
    let alog2: Log = Arc::default();
    let l = alog2.clone();
    let a2 = Core::start(da.path(), false, move |e| match e {
        Event::Peers(p) => l.lock().unwrap().peers = p,
        Event::Groups(g) => l.lock().unwrap().groups = g,
        _ => {}
    })
    .await
    .unwrap();

    until("bob drops the group when alice reconnects", || {
        implied(&blog).is_empty()
    })
    .await;
    assert!(!peer(&blog, &a2).unwrap().paired_us);
    a2.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unseen_news_per_group_until_looked_at() {
    let (a, alog, da) = node("n-alice").await;
    let (b, blog, _db) = node("n-bob").await;
    until("alice ticket", || !a.ticket().is_empty()).await;
    b.connect_ticket(&a.ticket()).await.unwrap();
    until("alice hears bob", || {
        peer(&alog, &b).is_some_and(|p| p.paired_us)
    })
    .await;
    a.pair(b.id());
    until("implied", || {
        implied(&alog).len() == 1 && implied(&blog).len() == 1
    })
    .await;
    let gid = implied(&alog).remove(0);
    until("both in the doc", || {
        b.group_view(&gid).is_some_and(|v| v.members.len() == 2)
    })
    .await;
    a.mark_all_seen();
    b.mark_all_seen();
    assert!(b.unseen().is_empty());

    // Alice posts a text, a file and edits the scratchpad.
    a.send_text(&gid, "first").unwrap();
    let file = da.path().join("note.txt");
    std::fs::write(&file, "hi").unwrap();
    a.offer_file(&gid, file, None, "file", false).await.unwrap();
    a.set_scratch(&gid, "pad text");
    until("bob has the news", || {
        let u = b.unseen();
        u.len() == 1 && u[0].texts.len() == 1 && u[0].files.len() == 1 && u[0].scratch
    })
    .await;
    let u = b.unseen().remove(0);
    assert_eq!(u.group, gid);
    assert_eq!(
        u.title, "n-alice",
        "an implied group is named after the other device"
    );
    assert_eq!(u.files[0].0, "note.txt");
    assert_eq!(u.texts[0].1, "first");
    // Alice has no news about her own posts.
    assert!(a.unseen().is_empty());

    // Looking at the group clears it; only later things are news again.
    b.mark_seen(&gid);
    assert!(b.unseen().is_empty());
    a.send_text(&gid, "second").unwrap();
    until("only the new text", || {
        let u = b.unseen();
        u.len() == 1
            && u[0].texts.len() == 1
            && u[0].texts[0].1 == "second"
            && u[0].files.is_empty()
            && !u[0].scratch
    })
    .await;

    b.shutdown().await;
    a.shutdown().await;
}
