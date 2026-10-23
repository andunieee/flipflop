//! Two cores in one process: connect by ticket, exchange names and a message; and (best effort)
//! see each other over mDNS.

use std::{sync::{Arc, Mutex}, time::Duration};

use flipflop::core::{Core, Event, Peer};
use tokio::time::{sleep, timeout};

type Log = Arc<Mutex<(Vec<Peer>, Vec<String>)>>;

async fn node(name: &str) -> (Core, Log, tempdir::Dir) {
    let dir = tempdir::Dir::new(name);
    let log: Log = Arc::default();
    let l = log.clone();
    let core = Core::start(dir.path(), false, move |e| match e {
        Event::Peers(p) => l.lock().unwrap().0 = p,
        Event::Message { text, mine: false, .. } => l.lock().unwrap().1.push(text),
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

#[tokio::test(flavor = "multi_thread")]
async fn connect_by_ticket_and_chat() {
    let _ = tracing_subscriber::fmt().with_env_filter("warn").try_init();
    let (a, alog, _da) = node("alice").await;
    let (b, blog, _db) = node("bob").await;

    until("alice ticket", || !a.ticket().is_empty()).await;
    b.connect_ticket(&a.ticket()).await.expect("connect");

    until("bob sees alice's hello name", || blog.lock().unwrap().0.iter().any(|p| p.connected && p.name == "alice")).await;
    until("alice sees bob's hello name", || alog.lock().unwrap().0.iter().any(|p| p.connected && p.name == "bob")).await;

    b.send(a.id(), "hi alice".into()).await.unwrap();
    until("message", || alog.lock().unwrap().1 == ["hi alice"]).await;

    // rename propagates over the live connection
    a.set_name("alicia");
    until("rename", || blog.lock().unwrap().0.iter().any(|p| p.name == "alicia")).await;

}

/// Peers on the same network show up via mDNS (with their names) and connect by id alone.
#[tokio::test(flavor = "multi_thread")]
async fn lan_discovery_then_connect_by_id() {
    let (a, alog, _da) = node("lan-alice").await;
    let (b, blog, _db) = node("lan-bob").await;

    until("bob sees alice on LAN", || blog.lock().unwrap().0.iter().any(|p| p.lan && p.id == a.id() && p.name == "lan-alice")).await;
    until("alice sees bob on LAN", || alog.lock().unwrap().0.iter().any(|p| p.lan && p.id == b.id() && p.name == "lan-bob")).await;

    b.connect(a.id()).await.expect("connect by id via mDNS addresses");
    b.send(a.id(), "over lan".into()).await.unwrap();
    until("message", || alog.lock().unwrap().1 == ["over lan"]).await;
}

mod tempdir {
    pub struct Dir(std::path::PathBuf);
    impl Dir {
        pub fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("flipflop-test-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            Dir(p)
        }
        pub fn path(&self) -> &std::path::Path { &self.0 }
    }
    impl Drop for Dir {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }
}
