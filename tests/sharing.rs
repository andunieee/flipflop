//! Group content between nodes in one process: texts, scratchpad, file offers and downloads.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use flipflop::core::{Core, MemberState};
use tokio::time::{sleep, timeout};

struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Self {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p =
            std::env::temp_dir().join(format!("flipflop-share-{tag}-{}-{n}", std::process::id()));
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
    core.set_download_dir(dir.path().join("downloads"));
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

/// `a` creates a group and invites `b`, who accepts. Returns the group id once both are joined.
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
async fn texts_and_scratchpad_sync() {
    let (a, _da) = node("alice").await;
    let (b, _db) = node("bob").await;
    let gid = group_of(&a, &b).await;

    b.send_text(&gid, "https://example.com/x?y=1").unwrap();
    a.send_text(&gid, "code 4711").unwrap();
    until("texts on both sides", || {
        [&a, &b]
            .iter()
            .all(|c| c.group_view(&gid).unwrap().texts.len() == 2)
    })
    .await;
    let view = a.group_view(&gid).unwrap();
    // newest first, each labelled with the device that sent it
    assert_eq!(
        (view.texts[0].name.as_str(), view.texts[0].text.as_str()),
        ("alice", "code 4711")
    );
    assert_eq!(
        (view.texts[1].name.as_str(), view.texts[1].mine),
        ("bob", false)
    );
    assert!(
        b.delete_text(&gid, &view.texts[0].id).is_err(),
        "only the sender removes a snippet"
    );
    b.delete_text(&gid, &view.texts[1].id).unwrap();
    until("deleted", || a.group_view(&gid).unwrap().texts.len() == 1).await;

    a.set_scratch(&gid, "shopping: milk");
    until("scratch", || {
        b.scratch(&gid).as_deref() == Some("shopping: milk")
    })
    .await;
    a.set_scratch(&gid, "shopping: milk, eggs");
    b.set_scratch(&gid, "list — shopping: milk");
    until("merged scratch", || {
        let (x, y) = (a.scratch(&gid).unwrap(), b.scratch(&gid).unwrap());
        x == y && x == "list — shopping: milk, eggs"
    })
    .await;

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn offered_files_are_downloaded_automatically_and_stopped_when_everyone_has_them() {
    let (a, da) = node("alice").await;
    let (b, _db) = node("bob").await;
    let gid = group_of(&a, &b).await;
    a.set_group_settings(&gid, None, Some(true), None);

    let src = da.path().join("hello.txt");
    let content: Vec<u8> = (0..300_000u32).flat_map(|i| i.to_le_bytes()).collect();
    std::fs::write(&src, &content).unwrap();
    let fid = a.offer_file(&gid, src, None, "file", false).await.unwrap();

    until("bob downloaded it", || {
        b.group_view(&gid)
            .unwrap()
            .files
            .first()
            .is_some_and(|f| f.have)
    })
    .await;
    let saved = b.group_view(&gid).unwrap().files[0]
        .saved_to
        .clone()
        .unwrap();
    assert_eq!(std::fs::read(&saved).unwrap(), content);
    assert_eq!(saved.file_name().unwrap(), "hello.txt");

    // alice's auto-stop kicks in once bob (everybody else) has it
    until("stopped", || a.group_view(&gid).unwrap().files[0].stopped).await;
    until("bob sees it stopped", || {
        b.group_view(&gid).unwrap().files[0].stopped
    })
    .await;
    let view = a.group_view(&gid).unwrap();
    assert_eq!(view.files[0].downloaded_by, ["bob"]);
    assert!(a.stop_file(&gid, &fid).is_ok());

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_download_and_manual_stop() {
    let (a, da) = node("alice").await;
    let (b, _db) = node("bob").await;
    let gid = group_of(&a, &b).await;
    b.set_group_settings(&gid, Some(false), None, None);

    let src = da.path().join("report.pdf");
    std::fs::write(&src, b"%PDF-fake").unwrap();
    let fid = a
        .offer_file(&gid, src.clone(), None, "file", false)
        .await
        .unwrap();
    until("offer visible", || {
        b.group_view(&gid).unwrap().files.len() == 1
    })
    .await;
    sleep(Duration::from_secs(2)).await;
    assert!(
        !b.group_view(&gid).unwrap().files[0].have,
        "auto download is off"
    );

    let path = b.download_file(&gid, &fid).await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"%PDF-fake");
    until("alice sees the download", || {
        a.group_view(&gid).unwrap().files[0].downloaded_by == ["bob"]
    })
    .await;
    assert!(
        a.group_view(&gid).unwrap().files[0].stopped == false,
        "auto-stop is off by default"
    );

    // a second offer, stopped before anyone downloads it
    let fid2 = a.offer_file(&gid, src, None, "file", false).await.unwrap();
    until("second offer", || {
        b.group_view(&gid).unwrap().files.len() == 2
    })
    .await;
    a.stop_file(&gid, &fid2).unwrap();
    until("stopped", || b.group_view(&gid).unwrap().files[0].stopped).await;
    assert!(b.download_file(&gid, &fid2).await.is_err());
    assert!(
        b.stop_file(&gid, &fid).is_err(),
        "only the sharer stops sharing"
    );

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn members_find_each_other_through_the_group() {
    let (a, _da) = node("alice").await;
    let (b, _db) = node("bob").await;
    let (c, _dc) = node("carol").await;
    let gid = group_of(&a, &b).await;
    c.connect_ticket(&a.ticket()).await.unwrap();
    a.add_to_group(&gid, c.id());
    until("invite", || !c.groups().invites.is_empty()).await;
    c.accept_invite(a.id(), &gid);

    until("all three know all three", || {
        [&a, &b, &c]
            .iter()
            .all(|n| n.group_view(&gid).is_some_and(|v| v.members.len() == 3))
    })
    .await;
    // bob and carol were never introduced directly: they reach each other through the document
    c.send_text(&gid, "hello from carol").unwrap();
    until("bob gets carol's text", || {
        b.group_view(&gid)
            .unwrap()
            .texts
            .iter()
            .any(|t| t.name == "carol")
    })
    .await;
    until("bob and carol connected", || {
        b.peers().iter().any(|p| p.id == c.id() && p.connected)
    })
    .await;

    for n in [&a, &b, &c] {
        n.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn tabs_are_shared_and_apps_reach_everyone() {
    let (a, da) = node("alice").await;
    let (b, db) = node("bob").await;
    let gid = group_of(&a, &b).await;
    // Apps are fetched whatever the files settings.
    b.set_group_settings(&gid, Some(false), None, None);
    let tabs = |c: &Core| -> Vec<(String, bool)> {
        c.group_view(&gid)
            .unwrap()
            .tabs
            .iter()
            .map(|t| (t.title.clone(), t.on))
            .collect()
    };

    a.set_tab(&gid, "texts", false).unwrap();
    assert!(a.set_tab(&gid, "nope", false).is_err());
    until("bob sees texts hidden", || {
        tabs(&b)
            == [("Files", true), ("Texts", false), ("Scratchpad", true)]
                .map(|(t, on)| (t.to_owned(), on))
    })
    .await;

    let wasm = b"\0asm\x01\0\0\0".to_vec();
    let src = da.path().join("chess.wasm");
    std::fs::write(&src, &wasm).unwrap();
    assert!(
        a.add_app(&gid, da.path().join("missing.wasm"), None)
            .await
            .is_err()
    );
    let not_wasm = da.path().join("notes.wasm");
    std::fs::write(&not_wasm, b"hello").unwrap();
    assert!(a.add_app(&gid, not_wasm, None).await.is_err());
    let id = a.add_app(&gid, src, None).await.unwrap();

    until("bob fetched the app", || {
        b.group_view(&gid).unwrap().tabs.iter().any(|t| {
            t.id == id && t.on && t.title == "chess" && t.app.as_ref().is_some_and(|a| a.ready)
        })
    })
    .await;
    let hash = blake3::hash(&wasm).to_hex().to_string();
    assert_eq!(
        std::fs::read(db.path().join("apps").join(format!("{hash}.wasm"))).unwrap(),
        wasm
    );
    // An app's file is not one of the group's files.
    assert!(a.group_view(&gid).unwrap().files.is_empty());
    assert!(b.group_view(&gid).unwrap().files.is_empty());

    b.set_tab(&gid, &id, false).unwrap();
    until("alice sees the app hidden", || {
        tabs(&a).last() == Some(&("chess".to_owned(), false))
    })
    .await;

    a.shutdown().await;
    b.shutdown().await;
}
