use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use flipflop::core::{Core, Listening, MemberState, clock_ms};
use tokio::time::{sleep, timeout};

struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Self {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p =
            std::env::temp_dir().join(format!("flipflop-listen-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
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

async fn node(name: &str) -> (Core, Dir) {
    let dir = Dir::new(name);
    let core = Core::start(dir.path(), false, |_| {}).await.unwrap();
    core.set_name(name);
    (core, dir)
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    timeout(Duration::from_secs(30), async {
        while !f() {
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

async fn group_of(a: &Core, b: &Core) -> String {
    until("ticket", || !a.ticket().is_empty()).await;
    b.connect_ticket(&a.ticket()).await.expect("connect");
    let gid = a.create_group();
    a.add_to_group(&gid, b.id());
    until("invite", || !b.groups().invites.is_empty()).await;
    b.accept_invite(a.id(), &gid);
    until("accepted", || {
        a.groups()
            .groups
            .iter()
            .any(|g| g.id == gid && g.members.iter().all(|m| m.state == MemberState::Accepted))
    })
    .await;
    until("both members in both docs", || {
        [a, b]
            .iter()
            .all(|c| c.group_view(&gid).is_some_and(|v| v.members.len() == 2))
    })
    .await;
    gid
}

#[tokio::test(flavor = "multi_thread")]
async fn listen_along() {
    let (a, _da) = node("alice").await;
    let (b, _db) = node("bob").await;
    let gid = group_of(&a, &b).await;

    let at = clock_ms();
    a.share_listen(
        &gid,
        Some(Listening {
            fid: "song".into(),
            track: 0,
            name: "Song.opus".into(),
            from_name: "you".into(),
            position_ms: 30_000,
            at_ms: at,
            paused: false,
        }),
    );
    until("bob hears alice", || b.listening(&gid).0.len() == 1).await;
    let heard = b.listening(&gid).0.remove(0);
    assert_eq!((heard.sharer, heard.name.as_str()), (a.id(), "alice"));
    assert_eq!(heard.now.fid, "song");
    // Same machine, same clock: whatever the offset estimate, it is small.
    assert!(
        (heard.now.at_ms - at).abs() < 200,
        "{} vs {at}",
        heard.now.at_ms
    );

    b.follow(&gid, a.id(), true);
    until("alice sees bob along", || a.listening(&gid).1 == ["bob"]).await;
    b.follow(&gid, a.id(), false);
    until("bob left", || a.listening(&gid).1.is_empty()).await;

    a.share_listen(&gid, None);
    until("alice stopped", || b.listening(&gid).0.is_empty()).await;

    // Said once and never again: it expires.
    a.share_listen(
        &gid,
        Some(Listening {
            fid: "song".into(),
            track: 0,
            name: "Song.opus".into(),
            from_name: "you".into(),
            position_ms: 0,
            at_ms: clock_ms(),
            paused: false,
        }),
    );
    until("heard once", || b.listening(&gid).0.len() == 1).await;
    until("expired", || b.listening(&gid).0.is_empty()).await;

    // Shared again, then gone with the connection.
    a.share_listen(
        &gid,
        Some(Listening {
            fid: "song".into(),
            track: 0,
            name: "Song.opus".into(),
            from_name: "you".into(),
            position_ms: 0,
            at_ms: clock_ms(),
            paused: true,
        }),
    );
    until("shared again", || b.listening(&gid).0.len() == 1).await;
    a.shutdown().await;
    until("gone with alice", || b.listening(&gid).0.is_empty()).await;
    b.shutdown().await;
}
