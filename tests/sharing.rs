//! Group content between nodes in one process: texts, scratchpad, file offers and downloads.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use flipflop::core::{Core, Event, GroupView, MemberState, TransfersNow};
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

    // a second offer of it once the first one is stopped, stopped before anyone downloads it
    a.stop_file(&gid, &fid).unwrap();
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

    // offered again: the same entry is back, and downloadable
    assert!(a.group_view(&gid).unwrap().files[0].can_reoffer);
    assert!(b.group_view(&gid).unwrap().files[0].can_reoffer == false);
    assert_eq!(a.reoffer_file(&gid, &fid2).await.unwrap(), fid2);
    until("offered again", || {
        !b.group_view(&gid).unwrap().files[0].stopped
    })
    .await;
    // the older offer of the same file can't come back next to it
    assert!(!a.group_view(&gid).unwrap().files[1].can_reoffer);
    assert!(a.reoffer_file(&gid, &fid).await.is_err());
    assert_eq!(b.group_view(&gid).unwrap().files.len(), 2);
    let path2 = b.download_file(&gid, &fid2).await.unwrap();
    assert_eq!(std::fs::read(&path2).unwrap(), b"%PDF-fake");

    // changed in the meantime: offered as a new file
    a.stop_file(&gid, &fid2).unwrap();
    std::fs::write(da.path().join("report.pdf"), b"%PDF-fake v2").unwrap();
    let fid3 = a.reoffer_file(&gid, &fid2).await.unwrap();
    assert_ne!(fid3, fid2);
    assert!(!a.group_view(&gid).unwrap().files[1].can_reoffer);

    // gone: cannot be offered again
    a.stop_file(&gid, &fid3).unwrap();
    std::fs::remove_file(da.path().join("report.pdf")).unwrap();
    assert!(!a.group_view(&gid).unwrap().files[0].can_reoffer);
    assert!(a.reoffer_file(&gid, &fid3).await.is_err());

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_are_shared_again_and_files_leave_the_tab() {
    let (a, da) = node("alice").await;
    let (b, _db) = node("bob").await;
    let gid = group_of(&a, &b).await;
    b.set_group_settings(&gid, Some(false), None, None);

    let src = da.path().join("notes.txt");
    std::fs::write(&src, b"notes").unwrap();
    let fid = a
        .offer_file(&gid, src.clone(), None, "file", false)
        .await
        .unwrap();
    assert_eq!(a.group_view(&gid).unwrap().files[0].shared_from, Some(src));
    assert!(
        a.remove_file(&gid, &fid).is_err(),
        "still shared: stop sharing first"
    );
    until("offer visible", || {
        b.group_view(&gid).unwrap().files.len() == 1
    })
    .await;
    assert!(b.group_view(&gid).unwrap().files[0].shared_from.is_none());
    b.download_file(&gid, &fid).await.unwrap();

    // the sender stops: whoever downloaded it can offer it again, as their own
    a.stop_file(&gid, &fid).unwrap();
    until("stopped", || b.group_view(&gid).unwrap().files[0].stopped).await;
    assert!(b.group_view(&gid).unwrap().files[0].can_reoffer);
    let again = b.reoffer_file(&gid, &fid).await.unwrap();
    assert_ne!(again, fid);
    until("alice sees bob's offer", || {
        a.group_view(&gid)
            .unwrap()
            .files
            .iter()
            .any(|f| f.id == again && f.from_name == "bob" && !f.stopped)
    })
    .await;
    let old = |c: &Core| {
        c.group_view(&gid)
            .unwrap()
            .files
            .into_iter()
            .find(|f| f.id == fid)
    };
    assert!(!old(&b).unwrap().can_reoffer, "already offered again");
    assert!(b.reoffer_file(&gid, &fid).await.is_err());

    // removed from bob's tab only
    assert!(old(&b).unwrap().can_remove);
    b.remove_file(&gid, &fid).unwrap();
    assert!(old(&b).is_none());
    assert!(old(&a).is_some());
    assert!(b.remove_file(&gid, &again).is_err(), "bob shares it");

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
            == [
                ("Files", true),
                ("Texts", false),
                ("Scratchpad", true),
                ("Music", false),
            ]
            .map(|(t, on)| (t.to_owned(), on))
    })
    .await;

    let wasm = b"\0asm\x01\0\0\0".to_vec();
    let src = da.path().join("chess.wasm");
    std::fs::write(&src, &wasm).unwrap();
    assert!(a.prepare_app(da.path().join("missing.wasm")).await.is_err());
    let not_wasm = da.path().join("notes.wasm");
    std::fs::write(&not_wasm, b"hello").unwrap();
    assert!(a.prepare_app(not_wasm).await.is_err());
    let draft = a.prepare_app(src).await.unwrap();
    assert_eq!(draft.form, Default::default());
    let id = a.add_app(&gid, &draft, &[]).await.unwrap();
    // Until the app names its tab, it is called after its file and hash.
    let named = format!("chess {}", &draft.hash[..6]);

    until("bob fetched the app", || {
        b.group_view(&gid).unwrap().tabs.iter().any(|t| {
            t.id == id && t.on && t.title == named && t.app.as_ref().is_some_and(|a| a.ready)
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

    // Bob adds the app he fetched to a group of his own, from his copy.
    let saved = b.saved_apps();
    assert_eq!(saved.len(), 1);
    assert_eq!(
        (
            saved[0].hash.as_str(),
            saved[0].name.as_str(),
            saved[0].file_name.as_str()
        ),
        (hash.as_str(), named.as_str(), "chess.wasm")
    );
    assert_eq!(saved[0].groups, [gid.clone()]);
    let other = b.create_group();
    let again = b.prepare_saved_app(&saved[0]).await.unwrap();
    assert_eq!(
        (again.hash.as_str(), again.size),
        (hash.as_str(), draft.size)
    );
    let reused = b.add_app(&other, &again, &[]).await.unwrap();
    assert!(
        b.group_view(&other).unwrap().tabs.iter().any(|t| {
            t.id == reused && t.app.as_ref().is_some_and(|a| a.ready && a.hash == hash)
        })
    );
    let saved = b.saved_apps();
    assert_eq!(saved.len(), 1, "one wasm, in two groups");
    assert_eq!(saved[0].groups.len(), 2);

    b.set_tab(&gid, &id, false).unwrap();
    until("alice sees the app hidden", || {
        tabs(&a).last() == Some(&(named.clone(), false))
    })
    .await;
    b.delete_app(&gid, &id).unwrap();
    until("alice sees the app gone", || {
        a.group_view(&gid).unwrap().tabs.iter().all(|t| t.id != id)
    })
    .await;

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn apps_are_downloaded_from_links() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (a, _da) = node("alice").await;
    let wasm = b"\0asm\x01\0\0\0".to_vec();
    // Serves the wasm at /dist/poll.wasm, anything else is a 404.
    let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = server.local_addr().unwrap();
    let body = wasm.clone();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = server.accept().await.unwrap();
            let mut head = vec![0; 4096];
            let n = stream.read(&mut head).await.unwrap();
            let head = String::from_utf8_lossy(&head[..n]).into_owned();
            let reply = if head.starts_with("GET /dist/poll.wasm ") {
                let mut r = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .into_bytes();
                r.extend_from_slice(&body);
                r
            } else {
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
            };
            stream.write_all(&reply).await.unwrap();
        }
    });

    let draft = a
        .prepare_app_url(&format!("http://{addr}/dist/poll.wasm"))
        .await
        .unwrap();
    assert_eq!(draft.file_name, "poll.wasm");
    assert_eq!(draft.hash, blake3::hash(&wasm).to_hex().to_string());
    assert!(
        a.prepare_app_url(&format!("http://{addr}/gone.wasm"))
            .await
            .is_err()
    );
    assert!(a.prepare_app_url("file:///etc/passwd").await.is_err());
    assert!(a.prepare_app_url("not a link").await.is_err());
    let gid = a.create_group();
    a.add_app(&gid, &draft, &[]).await.unwrap();
    assert_eq!(a.saved_apps().len(), 1);

    a.shutdown().await;
}

/// A plain HTTP/1.1 GET (with an optional Range): the response head and the body.
async fn http_get(url: &str, range: Option<&str>) -> (String, Vec<u8>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let rest = url.strip_prefix("http://").unwrap();
    let (host, path) = rest.split_at(rest.find('/').unwrap());
    let mut stream = tokio::net::TcpStream::connect(host).await.unwrap();
    let range = range.map(|r| format!("Range: {r}\r\n")).unwrap_or_default();
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n{range}Connection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).await.unwrap();
    let split = reply.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    (
        String::from_utf8_lossy(&reply[..split]).to_lowercase(),
        reply[split + 4..].to_vec(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn music_is_streamed_not_downloaded() {
    let (a, da) = node("alice").await;
    let (b, db) = node("bob").await;
    let gid = group_of(&a, &b).await;
    let tabs = |c: &Core| -> Vec<(String, bool)> {
        let v = c.group_view(&gid).unwrap();
        v.tabs.iter().map(|t| (t.id.clone(), t.on)).collect()
    };
    // The music tab is there, hidden, until someone shows it.
    assert!(tabs(&b).contains(&("music".to_owned(), false)));
    a.set_tab(&gid, "music", true).unwrap();
    until("bob sees the music tab", || {
        tabs(&b).contains(&("music".to_owned(), true))
    })
    .await;

    let song: Vec<u8> = (0..300_000u32).flat_map(|i| i.to_le_bytes()).collect();
    let src = da.path().join("song.ogg");
    std::fs::write(&src, &song).unwrap();
    // Only songs.
    let video = da.path().join("clip.mp4");
    std::fs::write(&video, b"not a song").unwrap();
    assert!(a.offer_music(&gid, video, None, false).await.is_err());
    let fid = a.offer_music(&gid, src.clone(), None, false).await.unwrap();
    until("bob sees the song", || {
        b.group_view(&gid)
            .unwrap()
            .music
            .iter()
            .any(|f| f.id == fid)
    })
    .await;
    // Not a file: it is not listed with them, and nothing downloads it (auto-download is on).
    assert!(a.group_view(&gid).unwrap().files.is_empty());
    assert!(b.group_view(&gid).unwrap().files.is_empty());
    assert!(b.download_file(&gid, &fid).await.is_err());

    // Ours plays from where it is, theirs from them: the same way, over HTTP with ranges.
    for (who, core) in [("alice", &a), ("bob", &b)] {
        let url = core.music_source(&gid, &fid, 0).await.unwrap();
        assert!(
            url.starts_with("http://127.0.0.1:") && url.ends_with("/song.ogg"),
            "{who}: {url}"
        );
        let (head, body) = http_get(&url, None).await;
        assert!(head.starts_with("http/1.1 200"), "{who}: {head}");
        assert!(head.contains("content-type: audio/ogg"), "{who}: {head}");
        assert!(body == song, "{who}");
        let (head, body) = http_get(&url, Some("bytes=1000-1999")).await;
        assert!(head.starts_with("http/1.1 206"), "{who}: {head}");
        assert!(
            head.contains("content-range: bytes 1000-1999/1200000"),
            "{who}: {head}"
        );
        assert_eq!(body, song[1000..2000], "{who}");
        let (head, body) = http_get(&url, Some("bytes=1100000-")).await;
        assert!(head.starts_with("http/1.1 206"), "{who}: {head}");
        assert_eq!(body, song[1_100_000..], "{who}");
        let (head, _) = http_get(&url, Some("bytes=5000000-")).await;
        assert!(head.starts_with("http/1.1 416"), "{who}: {head}");
        // Only with the token.
        let mut parts: Vec<&str> = url.split('/').collect();
        parts[3] = "nope";
        let (head, _) = http_get(&parts.join("/"), None).await;
        assert!(head.starts_with("http/1.1 404"), "{who}: {head}");
    }
    assert!(!db.path().join("downloads").join("song.ogg").exists());

    a.stop_file(&gid, &fid).unwrap();
    until("bob sees it gone", || {
        b.group_view(&gid).unwrap().music.is_empty()
    })
    .await;
    assert!(b.music_source(&gid, &fid, 0).await.is_err());

    // A folder is one album: its songs play one by one, as ranges of the folder's stream.
    let album = da.path().join("Album");
    std::fs::create_dir_all(album.join("cd2")).unwrap();
    let one: Vec<u8> = (0..50_000u32).flat_map(|i| (i * 3).to_le_bytes()).collect();
    let two: Vec<u8> = (0..70_000u32).flat_map(|i| (i * 7).to_le_bytes()).collect();
    std::fs::write(album.join("01 one.mp3"), &one).unwrap();
    std::fs::write(album.join("cover.jpg"), b"not a song").unwrap();
    std::fs::write(album.join("cd2").join("02 two.flac"), &two).unwrap();
    let empty = da.path().join("Empty");
    std::fs::create_dir_all(&empty).unwrap();
    std::fs::write(empty.join("notes.txt"), b"no songs").unwrap();
    assert!(a.offer_music(&gid, empty, None, false).await.is_err());
    let aid = a.offer_music(&gid, album, None, false).await.unwrap();
    until("bob sees the album", || {
        b.group_view(&gid)
            .unwrap()
            .music
            .iter()
            .any(|f| f.id == aid && f.tracks == ["01 one.mp3", "cd2/02 two.flac"])
    })
    .await;
    assert!(b.download_file(&gid, &aid).await.is_err());
    for (who, core) in [("alice", &a), ("bob", &b)] {
        for (track, bytes) in [(0, &one), (1, &two)] {
            let url = core.music_source(&gid, &aid, track).await.unwrap();
            let (head, body) = http_get(&url, None).await;
            assert!(head.starts_with("http/1.1 200"), "{who} {track}: {head}");
            assert!(body == *bytes, "{who} {track}");
            let (head, body) = http_get(&url, Some("bytes=5000-9999")).await;
            assert!(head.starts_with("http/1.1 206"), "{who} {track}: {head}");
            assert_eq!(body, bytes[5000..10000], "{who} {track}");
        }
        assert!(core.music_source(&gid, &aid, 2).await.is_err());
    }
    let url = b.music_source(&gid, &aid, 1).await.unwrap();
    assert!(url.ends_with("/1/02%20two.flac"), "{url}");
    let (head, _) = http_get(&url, None).await;
    assert!(head.contains("content-type: audio/flac"), "{head}");

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn folders_arrive_whole() {
    let (a, da) = node("alice").await;
    let (b, _db) = node("bob").await;
    let gid = group_of(&a, &b).await;

    let src = da.path().join("photos");
    std::fs::create_dir_all(src.join("2026/october")).unwrap();
    std::fs::create_dir_all(src.join("empty")).unwrap();
    let big: Vec<u8> = (0..200_000u32).flat_map(|i| i.to_le_bytes()).collect();
    std::fs::write(src.join("2026/october/big.bin"), &big).unwrap();
    std::fs::write(src.join("readme.txt"), b"hello").unwrap();
    std::fs::write(src.join("zero"), b"").unwrap();
    let fid = a.offer_file(&gid, src, None, "file", false).await.unwrap();

    until("bob downloaded it", || {
        b.group_view(&gid)
            .unwrap()
            .files
            .first()
            .is_some_and(|f| f.have)
    })
    .await;
    let view = b.group_view(&gid).unwrap();
    let f = &view.files[0];
    assert_eq!((f.id.as_str(), f.kind.as_str()), (fid.as_str(), "folder"));
    assert_eq!(f.size, big.len() as u64 + 5);
    let saved = f.saved_to.clone().unwrap();
    assert_eq!(saved.file_name().unwrap(), "photos");
    assert_eq!(
        std::fs::read(saved.join("2026/october/big.bin")).unwrap(),
        big
    );
    assert_eq!(std::fs::read(saved.join("readme.txt")).unwrap(), b"hello");
    assert_eq!(std::fs::read(saved.join("zero")).unwrap(), b"");
    assert!(saved.join("empty").is_dir());
    assert!(
        !saved.with_file_name(format!(".{fid}.part")).exists(),
        "no leftovers"
    );

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_resume_from_what_already_arrived() {
    let (a, da) = node("alice").await;
    let (b, _db) = node("bob").await;
    let gid = group_of(&a, &b).await;
    b.set_group_settings(&gid, Some(false), None, None);

    let content: Vec<u8> = (0..400_000u32).flat_map(|i| i.to_le_bytes()).collect();
    let src = da.path().join("resume.bin");
    std::fs::write(&src, &content).unwrap();
    let fid = a.offer_file(&gid, src, None, "file", false).await.unwrap();
    until("offer visible", || {
        b.group_view(&gid)
            .unwrap()
            .files
            .iter()
            .any(|f| f.id == fid)
    })
    .await;

    // As if an earlier attempt had been cut off after this many bytes.
    std::fs::create_dir_all(b.download_dir()).unwrap();
    let part = b.download_dir().join(format!(".{fid}.part"));
    std::fs::write(&part, &content[..123_456]).unwrap();
    let saved = b.download_file(&gid, &fid).await.unwrap();
    assert_eq!(std::fs::read(&saved).unwrap(), content);
    assert!(!part.exists(), "the part is gone once it is whole");

    // A folder's stream is its listing followed by its files, and resumes the same way.
    let folder = da.path().join("resume-folder");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("a.txt"), b"hello").unwrap();
    std::fs::write(folder.join("b.bin"), &content[..200_000]).unwrap();
    let fid = a
        .offer_file(&gid, folder, None, "file", false)
        .await
        .unwrap();
    until("folder offer visible", || {
        b.group_view(&gid)
            .unwrap()
            .files
            .iter()
            .any(|f| f.id == fid)
    })
    .await;
    let listing = format!(
        r#"{{"dirs":[],"files":[["a.txt",5],["b.bin",{}]]}}"#,
        200_000
    );
    let mut prefix = (listing.len() as u32).to_be_bytes().to_vec();
    prefix.extend(listing.as_bytes());
    prefix.extend(b"hel"); // 3 of a.txt's 5 bytes
    let part = b.download_dir().join(format!(".{fid}.part"));
    std::fs::write(&part, &prefix).unwrap();
    let saved = b.download_file(&gid, &fid).await.unwrap();
    assert_eq!(std::fs::read(saved.join("a.txt")).unwrap(), b"hello");
    assert_eq!(
        std::fs::read(saved.join("b.bin")).unwrap(),
        &content[..200_000]
    );
    assert!(!part.exists());

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_file_is_not_offered_twice() {
    let (a, da) = node("alice").await;
    let (b, db) = node("bob").await;
    let gid = group_of(&a, &b).await;
    b.set_group_settings(&gid, Some(false), None, None);
    let offer = |c: &Core, path: PathBuf, name: Option<&str>| {
        let (c, gid, name) = (c.clone(), gid.clone(), name.map(str::to_owned));
        async move { c.offer_file(&gid, path, name, "file", false).await }
    };

    let src = da.path().join("a.txt");
    std::fs::write(&src, b"same").unwrap();
    let fid = offer(&a, src.clone(), None).await.unwrap();
    let err = offer(&a, src.clone(), None).await.unwrap_err();
    assert_eq!(err.to_string(), "you already share a.txt");
    assert_eq!(
        a.group_view(&gid).unwrap().files.len(),
        1,
        "no second entry"
    );

    // a copy elsewhere is the same file
    let copy = da.path().join("copy");
    std::fs::create_dir_all(&copy).unwrap();
    std::fs::write(copy.join("a.txt"), b"same").unwrap();
    assert!(offer(&a, copy.join("a.txt"), None).await.is_err());

    // another member offering it too
    until("offer visible", || {
        b.group_view(&gid).unwrap().files.len() == 1
    })
    .await;
    let theirs = db.path().join("a.txt");
    std::fs::write(&theirs, b"same").unwrap();
    let err = offer(&b, theirs.clone(), None).await.unwrap_err();
    assert_eq!(err.to_string(), "alice already shares a.txt");

    // other bytes, or another name, is another file: two empty files aren't the same one
    std::fs::write(&src, b"changed").unwrap();
    offer(&a, src, None).await.unwrap();
    for name in ["x", "y"] {
        let empty = da.path().join(name);
        std::fs::write(&empty, b"").unwrap();
        offer(&a, empty, None).await.unwrap();
    }

    // a folder, too
    let folder = da.path().join("folder");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("f"), b"in a folder").unwrap();
    offer(&a, folder.clone(), None).await.unwrap();
    assert!(offer(&a, folder.clone(), None).await.is_err());

    // not offered anymore: fine again
    a.stop_file(&gid, &fid).unwrap();
    until("bob sees it stopped", || {
        b.group_view(&gid)
            .unwrap()
            .files
            .iter()
            .any(|f| f.id == fid && f.stopped)
    })
    .await;
    offer(&b, theirs, None).await.unwrap();

    a.shutdown().await;
    b.shutdown().await;
}

/// A node that records the views it publishes, which show even what lasts only a moment.
async fn recording_node(name: &str) -> (Core, Dir, Arc<Mutex<Vec<GroupView>>>) {
    let dir = Dir::new(name);
    let views: Arc<Mutex<Vec<GroupView>>> = Arc::default();
    let seen = views.clone();
    let core = Core::start(dir.path(), false, move |e| {
        if let Event::Group(v) = e {
            seen.lock().unwrap().push(v);
        }
    })
    .await
    .unwrap();
    core.set_name(name);
    core.set_download_dir(dir.path().join("downloads"));
    (core, dir, views)
}

#[tokio::test(flavor = "multi_thread")]
async fn offers_show_while_hashing_and_their_sender_sees_them_go() {
    let (a, da, views) = recording_node("alice").await;
    let (b, _db) = node("bob").await;
    let gid = group_of(&a, &b).await;
    b.set_group_settings(&gid, Some(false), None, None);

    let src = da.path().join("photos");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("one"), vec![1u8; 300_000]).unwrap();
    let fid = a.offer_file(&gid, src, None, "file", false).await.unwrap();

    let shown = views.lock().unwrap().clone();
    let first = shown
        .iter()
        .find(|v| v.files.iter().any(|f| f.preparing))
        .expect("it showed before it was hashed");
    let card = first.files.iter().find(|f| f.preparing).unwrap();
    assert_eq!(
        (card.name.as_str(), card.kind.as_str()),
        ("photos", "folder")
    );
    assert!(card.mine);
    let view = a.group_view(&gid).unwrap();
    assert_eq!(view.files.len(), 1, "the real offer took its place");
    assert_eq!(view.files[0].id, fid);
    assert!(!view.files[0].preparing);
    until("the offer reaches bob", || {
        b.group_view(&gid).unwrap().files.len() == 1
    })
    .await;

    views.lock().unwrap().clear();
    b.download_file(&gid, &fid).await.unwrap();
    until("the sending is over", || {
        a.group_view(&gid).unwrap().files[0].sending.is_empty()
    })
    .await;
    let shown = views.lock().unwrap().clone();
    let going = shown
        .iter()
        .flat_map(|v| &v.files)
        .flat_map(|f| &f.sending)
        .next()
        .expect("the sender saw it going");
    assert_eq!((going.to.as_str(), going.total), ("bob", 300_000));

    a.shutdown().await;
    b.shutdown().await;
}

/// A node that records what it says is moving.
async fn transfers_node(name: &str) -> (Core, Dir, Arc<Mutex<Vec<TransfersNow>>>) {
    let dir = Dir::new(name);
    let log: Arc<Mutex<Vec<TransfersNow>>> = Arc::default();
    let seen = log.clone();
    let core = Core::start(dir.path(), false, move |e| {
        if let Event::Transfers(now) = e {
            seen.lock().unwrap().push(now);
        }
    })
    .await
    .unwrap();
    core.set_name(name);
    core.set_download_dir(dir.path().join("downloads"));
    (core, dir, log)
}

#[tokio::test(flavor = "multi_thread")]
async fn both_sides_say_what_is_moving_until_it_is_done() {
    let (a, da, sender) = transfers_node("alice").await;
    let (b, _db, receiver) = transfers_node("bob").await;
    let gid = group_of(&a, &b).await;
    b.set_group_settings(&gid, Some(false), None, None);

    let src = da.path().join("clip.bin");
    std::fs::write(&src, vec![7u8; 300_000]).unwrap();
    let fid = a.offer_file(&gid, src, None, "file", false).await.unwrap();
    until("the hashing is over", || !a.transfers_now().active()).await;
    let preparing = sender.lock().unwrap().iter().any(|t| t.preparing == 1);
    assert!(preparing, "hashing counts");

    until("the offer reaches bob", || {
        b.group_view(&gid).unwrap().files.len() == 1
    })
    .await;
    b.download_file(&gid, &fid).await.unwrap();
    assert!(
        !b.transfers_now().active(),
        "over when the file is in place"
    );
    until("alice is done", || !a.transfers_now().active()).await;

    let receiving: fn(&TransfersNow) -> bool = |t| t.receiving == 1;
    let sending: fn(&TransfersNow) -> bool = |t| t.sending == 1;
    for (log, name, moving) in [(&receiver, "bob", receiving), (&sender, "alice", sending)] {
        let log = log.lock().unwrap();
        let during = log
            .iter()
            .find(|t| moving(t))
            .unwrap_or_else(|| panic!("{name} said so"));
        assert_eq!(during.name.as_deref(), Some("clip.bin"));
        assert_eq!(during.total, Some(300_000));
        assert_eq!(log.last(), Some(&TransfersNow::default()), "{name} ended");
    }

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_download_stays_on_hold_even_with_auto_download() {
    let (a, da) = node("alice").await;
    let (b, db) = node("bob").await;
    let gid = group_of(&a, &b).await;
    assert!(
        b.groups()
            .groups
            .iter()
            .any(|g| g.id == gid && g.auto_download),
        "bob gets everything automatically"
    );

    // Hard to compress, and big enough to catch it on its way.
    let mut content = vec![0u8; 96 << 20];
    blake3::Hasher::new()
        .update(b"seed")
        .finalize_xof()
        .fill(&mut content);
    let src = da.path().join("big.bin");
    std::fs::write(&src, &content).unwrap();
    let fid = a.offer_file(&gid, src, None, "file", false).await.unwrap();

    let file = |c: &Core| {
        c.group_view(&gid)
            .unwrap()
            .files
            .into_iter()
            .find(|f| f.id == fid)
    };
    until("the download started", || {
        file(&b).is_some_and(|f| f.progress.is_some() && f.can_hold)
    })
    .await;
    b.hold_file(&gid, &fid).unwrap();
    until("the download stopped", || {
        file(&b).unwrap().progress.is_none()
    })
    .await;
    let held = file(&b).unwrap();
    assert!(held.on_hold && !held.have && !held.can_hold);
    assert!(held.error.is_none(), "cancelled is not failed");
    let downloads = db.path().join("downloads");
    assert_eq!(
        std::fs::read_dir(&downloads).unwrap().count(),
        0,
        "nothing left behind"
    );
    until("alice sees it stop", || {
        file(&a).unwrap().sending.is_empty()
    })
    .await;

    // Whatever makes the automatic jobs look again, it stays that way.
    a.send_text(&gid, "wake up").unwrap();
    until("the text", || !b.group_view(&gid).unwrap().texts.is_empty()).await;
    sleep(Duration::from_secs(1)).await;
    let held = file(&b).unwrap();
    assert!(held.on_hold && held.progress.is_none());
    assert_eq!(file(&a).unwrap().downloaded_by.len(), 0);

    // Until bob asks for it.
    let path = b.download_file(&gid, &fid).await.unwrap();
    assert!(std::fs::read(&path).unwrap() == content);
    let done = file(&b).unwrap();
    assert!(done.have && !done.on_hold);
    assert!(b.hold_file(&gid, &fid).is_err(), "nothing left to hold");
    assert!(a.hold_file(&gid, &fid).is_err(), "not on your own file");

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn automatic_downloads_are_fetched_one_at_a_time() {
    let (a, da) = node("alice").await;
    let (b, _db) = node("bob").await;
    let gid = group_of(&a, &b).await;
    // Hold the offers back, so they are all waiting when the automatic jobs start.
    b.set_group_settings(&gid, Some(false), None, None);

    let mut names = Vec::new();
    for name in ["one.bin", "two.bin", "three.bin"] {
        // Hard to compress, and big enough that one can be caught moving.
        let mut content = vec![0u8; 32 << 20];
        blake3::Hasher::new()
            .update(name.as_bytes())
            .finalize_xof()
            .fill(&mut content);
        let path = da.path().join(name);
        std::fs::write(&path, content).unwrap();
        a.offer_file(&gid, path, None, "file", false).await.unwrap();
        names.push(name);
    }
    until("the offers reach bob", || {
        b.group_view(&gid).unwrap().files.len() == names.len()
    })
    .await;

    b.set_group_settings(&gid, Some(true), None, None);
    let mut seen_moving = false;
    let mut seen_queued = false;
    until("bob has them all", || {
        let now = b.transfers_now();
        seen_moving |= now.receiving > 0;
        seen_queued |= now.queued > 0
            && now
                .items
                .iter()
                .any(|t| t.queued && names.iter().any(|n| n == &t.name));
        assert!(now.receiving <= 1, "two files moved at once: {now:?}");
        b.group_view(&gid)
            .unwrap()
            .files
            .iter()
            .filter(|f| f.have)
            .count()
            == names.len()
    })
    .await;
    assert!(seen_moving, "nothing was ever moving");
    assert!(seen_queued, "the waiting files were never listed");

    a.shutdown().await;
    b.shutdown().await;
}
