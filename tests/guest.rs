//! Guest links over the LAN transport: a browser is played by a WebSocket client that keeps its own copy of the
//! group document, exactly as the page does.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use flipflop::{core::Core, groupdoc::GroupDoc};
use futures_util::{SinkExt, StreamExt};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::{sleep, timeout},
};
use tokio_tungstenite::tungstenite::Message;

struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("flipflop-guest-{}", std::process::id()));
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

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    timeout(Duration::from_secs(20), async {
        while !f() {
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

/// One HTTP exchange: (status, body).
async fn http(port: u16, method: &str, path: &str, body: &[u8]) -> (u16, Vec<u8>) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes()).await.unwrap();
    s.write_all(body).await.unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).await.unwrap();
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response head")
        + 4;
    let status = std::str::from_utf8(&raw[..split])
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    (status, raw[split..].to_vec())
}

type Ws = tokio_tungstenite::WebSocketStream<TcpStream>;

/// A page: its document and its connection.
struct Page {
    ws: Ws,
    doc: GroupDoc,
    me: String,
    name: String,
    uploads: bool,
}

impl Page {
    async fn open(port: u16, token: &str) -> Page {
        let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let (ws, _) =
            tokio_tungstenite::client_async(format!("ws://127.0.0.1:{port}/{token}/ws"), stream)
                .await
                .expect("handshake");
        let mut page = Page {
            ws,
            doc: GroupDoc::new(),
            me: String::new(),
            name: String::new(),
            uploads: false,
        };
        until_hello(&mut page).await;
        page
    }

    async fn pump(&mut self) {
        let mut frame;
        while let Some(m) = self.doc.sync_message("host") {
            frame = vec![1u8];
            frame.extend_from_slice(&m);
            self.ws.send(Message::Binary(frame.into())).await.unwrap();
        }
    }

    /// Process messages for a while.
    async fn listen(&mut self, ms: u64) {
        let _ = timeout(Duration::from_millis(ms), async {
            while let Some(Ok(msg)) = self.ws.next().await {
                if let Message::Binary(b) = msg {
                    self.handle(&b);
                    self.pump().await;
                }
            }
        })
        .await;
    }

    fn handle(&mut self, b: &[u8]) {
        match b[0] {
            0 => {
                let v: serde_json::Value = serde_json::from_slice(&b[1..]).unwrap();
                match v["t"].as_str() {
                    Some("hello") => {
                        self.me = v["id"].as_str().unwrap().into();
                        self.name = v["name"].as_str().unwrap().into();
                        self.uploads = v["uploads"].as_bool().unwrap();
                    }
                    Some("uploads") => self.uploads = v["on"].as_bool().unwrap(),
                    _ => {}
                }
            }
            1 => {
                self.doc.receive_sync("host", &b[1..]).unwrap();
            }
            _ => {}
        }
    }
}

async fn until_hello(page: &mut Page) {
    timeout(Duration::from_secs(10), async {
        while page.me.is_empty() {
            let Some(Ok(Message::Binary(b))) = page.ws.next().await else {
                panic!("no hello")
            };
            page.handle(&b);
            page.pump().await;
        }
    })
    .await
    .expect("hello");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guest_syncs_downloads_and_uploads_over_the_lan() {
    let dir = Dir::new();
    let host = Core::start(dir.path(), false, |_| {}).await.unwrap();
    host.set_name("host");
    host.set_download_dir(dir.path().join("downloads"));
    let gid = host.create_guest_link(true);
    let info = || {
        host.groups()
            .groups
            .into_iter()
            .find(|g| g.id == gid)
            .and_then(|g| g.guest)
            .unwrap()
    };
    until("the server", || info().lan_url.is_some()).await;
    let url = info().lan_url.unwrap();
    let port: u16 = url
        .split(':')
        .nth(2)
        .unwrap()
        .split('/')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let token = info().token;

    // Only the link's own paths answer.
    assert_eq!(http(port, "GET", "/nothing/", b"").await.0, 404);
    assert_eq!(http(port, "GET", &format!("/{token}/"), b"").await.0, 200);
    assert_eq!(
        http(port, "GET", &format!("/{token}/flipflop_web_bg.wasm"), b"")
            .await
            .0,
        200
    );

    // The guest gets a name and appears in the document.
    let mut page = Page::open(port, &token).await;
    assert!(page.name.contains('-'), "{}", page.name);
    assert!(page.uploads);
    page.listen(500).await;
    until("guest member", || {
        host.group_view(&gid)
            .unwrap()
            .members
            .iter()
            .any(|m| m.name == page.name && m.online)
    })
    .await;
    assert_eq!(
        info()
            .guests
            .iter()
            .filter(|g| g.name == page.name && g.online)
            .count(),
        1
    );

    // Texts and the scratchpad go both ways.
    page.doc
        .add_text(&page.me.clone(), &page.name.clone(), "from the guest");
    page.doc.set_scratch("guest pad");
    page.pump().await;
    until("guest text on the host", || {
        host.group_view(&gid)
            .unwrap()
            .texts
            .iter()
            .any(|t| t.text == "from the guest")
    })
    .await;
    until("guest pad on the host", || {
        host.scratch(&gid).as_deref() == Some("guest pad")
    })
    .await;
    host.send_text(&gid, "from the host").unwrap();
    host.set_scratch(&gid, "guest pad, host edit");
    for _ in 0..40 {
        if page.doc.texts().iter().any(|t| t.text == "from the host")
            && page.doc.scratch() == "guest pad, host edit"
        {
            break;
        }
        page.listen(250).await;
    }
    assert!(
        page.doc.texts().iter().any(|t| t.text == "from the host"),
        "host text never reached the page"
    );
    assert_eq!(page.doc.scratch(), "guest pad, host edit");

    // An upload becomes an offer from the guest, served by the host.
    let (status, body) = http(
        port,
        "PUT",
        &format!("/{token}/up?name=hello%20there.txt"),
        b"hello",
    )
    .await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let fid = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let offer = host
        .group_view(&gid)
        .unwrap()
        .files
        .into_iter()
        .find(|f| f.id == fid)
        .expect("offer");
    assert_eq!(
        (offer.name.as_str(), offer.size, offer.from_name.as_str()),
        ("hello there.txt", 5, page.name.as_str())
    );
    page.listen(500).await;
    assert!(page.doc.files().iter().any(|f| f.id == fid));

    // ... which the guest, or anyone with the link, can download; that is marked in the document.
    let (status, body) = http(port, "GET", &format!("/{token}/dl/{fid}"), b"").await;
    assert_eq!((status, body.as_slice()), (200, &b"hello"[..]));
    // (Here the guest that uploaded it is the one who downloads it: both come from 127.0.0.1.)
    for _ in 0..40 {
        if page
            .doc
            .files()
            .iter()
            .any(|f| f.id == fid && f.downloaded.get(&page.me).is_some_and(|ts| *ts > offer.ts))
        {
            break;
        }
        page.listen(250).await;
    }
    assert!(
        page.doc
            .files()
            .iter()
            .any(|f| f.id == fid && f.downloaded.get(&page.me).is_some_and(|ts| *ts >= offer.ts)),
        "download not marked"
    );
    assert_eq!(
        http(port, "GET", &format!("/{token}/dl/nope"), b"").await.0,
        404
    );

    // Uploads can be turned off, and the page is told.
    host.set_guest_uploads(&gid, false);
    page.listen(500).await;
    assert!(!page.uploads);
    assert_eq!(
        http(port, "PUT", &format!("/{token}/up?name=x.txt"), b"x")
            .await
            .0,
        403
    );

    // Closing the link disconnects the guest and the paths stop answering (the server stays while another link lives).
    let other = host.create_guest_link(false);
    host.close_guest_link(&gid);
    let closed = timeout(Duration::from_secs(5), async {
        loop {
            match page.ws.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return true,
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "the socket stayed open");
    assert!(host.groups().groups.iter().all(|g| g.id != gid));
    assert_eq!(http(port, "GET", &format!("/{token}/"), b"").await.0, 404);
    assert!(host.groups().groups.iter().any(|g| g.id == other));
}
