//! Browser link: a temporary web page on the local network, so someone
//! without flipflop can swap files and text with us from any browser.
//!
//! While a link is open the guest shows up as one more peer, "Browser", with
//! the usual send card, transfer cards and history. What we send becomes an
//! offer on the web page for the browser to download; what the page uploads
//! is saved like a peer's files, in `<downloads>/Browser`. Nothing is paired
//! or remembered: ending the link stops the server and withdraws whatever
//! wasn't downloaded yet. The link itself is the key, a random token in its
//! path; there are no prompts.
//!
//! The link comes in two forms. On the same network, the page is served
//! from here over plain HTTP. From anywhere else, it is a static page on the
//! web ([`WEB_PAGE`], built from `web/`) that runs iroh in the browser and
//! reaches us through our home relay, carrying the same HTTP requests
//! (`http::serve_tunnel`). Its URL holds the token and our address in the
//! fragment, which browsers never send to the page's host.
//!
//! The server only runs while the app does (no Android foreground service).

mod http;
mod zip;

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use hyper::body::Incoming;
use hyper::StatusCode;
use n0_future::task::AbortOnDropHandle;
use serde_json::json;
use slint::{ComponentHandle, Model, SharedString};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, watch};

use crate::app::{refresh_history, refresh_peers, show_selected, AppCtx};
use crate::emitter::apply_transfer_event;
use crate::engine::{TransferDirection, TransferPathType, TransferPeer, TransferStatus};
use crate::format::fmt_bytes;
use crate::platform::{copy_to_clipboard, toast};
use crate::recorder::{Ctx, Recorder};
use crate::transfers::{self, PASTE_FILE_NAME, PASTE_MAX_BYTES};
use crate::{android, AppWindow, Logic, PeerRow, State, TransferRow};
use http::Reply;

/// The guest's peer id: its transfer rows and history entries carry it.
pub(crate) const GUEST_ID: &str = "browser";
const GUEST_NAME: &str = "Browser";

/// The page on the web: `src/guest/page.html` plus iroh in wasm (`web/`),
/// published to GitHub Pages by `.github/workflows/pages.yml`.
const WEB_PAGE: &str = "https://andunieee.github.io/flipflop/";

/// Tried first, so links look alike from one session to the next; any free
/// port does if it's taken.
const PORT: u16 = 53530;
/// A browser counts as connected this long after its last request (the page
/// keeps a request open, so this only covers the gap between two).
const CONNECTED_FOR: Duration = Duration::from_secs(10);
/// An upload batch nothing arrived for in this long is given up.
const BATCH_IDLE: Duration = Duration::from_secs(90);
/// How often transfer rows hear about progress.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);
const CHUNK: usize = 256 * 1024;

pub(crate) fn is_guest(peer_id: &str) -> bool {
    peer_id == GUEST_ID
}

/// The open browser link, if any. Shared through `AppCtx`.
#[derive(Clone, Default)]
pub(crate) struct Guest {
    session: Arc<Mutex<Option<Session>>>,
    starting: Arc<AtomicBool>,
}

struct Session {
    shared: Arc<Shared>,
    port: u16,
    /// Our LAN addresses, likeliest to work first; `address` is the one shown.
    addresses: Vec<Ipv4Addr>,
    address: usize,
    /// Showing the link for the web (anywhere) rather than the LAN one.
    web: bool,
    _server: AbortOnDropHandle<()>,
    /// Takes the web page's connections; `None` without a node.
    _tunnel: Option<AbortOnDropHandle<()>>,
    _presence: AbortOnDropHandle<()>,
}

impl Session {
    fn url(&self, ctx: &AppCtx) -> Option<String> {
        if self.web {
            return self.web_url(ctx);
        }
        let ip = self.addresses.get(self.address)?;
        Some(format!("http://{ip}:{}/{}", self.port, self.shared.token))
    }

    /// `<page>#t=<token>&e=<endpoint id>&r=<relay url>`; `None` until we
    /// have a home relay (none at all with the relay disabled).
    fn web_url(&self, ctx: &AppCtx) -> Option<String> {
        self._tunnel.as_ref()?;
        let (endpoint_id, relay_url) = ctx.node()?.guest_address();
        Some(web_url(&self.shared.token, &endpoint_id, &relay_url?))
    }
}

fn web_url(token: &str, endpoint_id: &str, relay_url: &str) -> String {
    let fragment = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("t", token)
        .append_pair("e", endpoint_id)
        .append_pair("r", relay_url)
        .finish();
    format!("{WEB_PAGE}#{fragment}")
}

impl Guest {
    fn shared(&self) -> Option<Arc<Shared>> {
        self.session.lock().unwrap().as_ref().map(|s| s.shared.clone())
    }

    pub(crate) fn is_active(&self) -> bool {
        self.session.lock().unwrap().is_some()
    }
}

/// Server-side state of a link, shared by the HTTP handlers and the UI.
struct Shared {
    ctx: AppCtx,
    token: String,
    device_name: String,
    inner: Mutex<Inner>,
    /// Bumped whenever the page's content changes (wakes long polls).
    version: watch::Sender<u64>,
    presence: Mutex<Presence>,
}

#[derive(Default)]
struct Inner {
    /// Set once the link ended: nothing new gets added.
    closed: bool,
    offers: Vec<Offer>,
    /// Sends stopped while still being prepared.
    withdrawn: HashSet<String>,
    batches: HashMap<u64, Batch>,
    next_id: u64,
}

#[derive(Default)]
struct Presence {
    last: Option<Instant>,
    /// Long polls waiting right now.
    polling: usize,
    /// "Safari on iPhone"
    label: String,
}

/// Something we sent, waiting on the page.
struct Offer {
    id: u64,
    /// Its transfer row.
    key: String,
    content: Content,
    recorder: Arc<Recorder>,
    /// Android share-sheet files it serves.
    staged: Vec<PathBuf>,
    /// Downloaded (text: shown) once; its row is gone, but it stays on the
    /// page until the link ends.
    done: bool,
    /// Files of a multi-file offer downloaded one by one.
    files_done: HashSet<usize>,
}

enum Content {
    Text(String),
    Files(Files),
}

#[derive(Clone)]
struct Files {
    /// "report.pdf", "photos", "3 items"
    title: String,
    /// The file's own name, or a zip's.
    download_name: String,
    entries: Vec<zip::Entry>,
    /// Bytes of the files themselves.
    size: u64,
    /// One plain file: served as is.
    single: bool,
    /// Several plain files: each can also be downloaded on its own.
    loose: bool,
}

/// Files the page is uploading, announced before the first byte.
struct Batch {
    key: String,
    files: Vec<Upload>,
    total: u64,
    /// Bytes of the files already saved.
    received: u64,
    save_dir: PathBuf,
    recorder: Arc<Recorder>,
    saved: Vec<String>,
    conflicts: Vec<serde_json::Value>,
    started: Instant,
    last_activity: Instant,
    /// Tells a running upload to stop.
    cancelled: Arc<AtomicBool>,
}

struct Upload {
    /// Relative path, cleaned up (`/`-separated).
    name: String,
    size: u64,
    done: bool,
}

#[derive(serde::Deserialize)]
struct BatchRequest {
    files: Vec<BatchFile>,
}

#[derive(serde::Deserialize)]
struct BatchFile {
    name: String,
    size: u64,
}

/// Why a batch ended early.
#[derive(Clone, Copy, PartialEq)]
enum Ended {
    /// Cancel on our transfer card.
    CancelledHere,
    ByBrowser,
    Stalled,
    LinkClosed,
}

fn guest_peer() -> TransferPeer {
    TransferPeer {
        endpoint_id: GUEST_ID.to_string(),
        display_name: Some(GUEST_NAME.to_string()),
        device_type: None,
    }
}

/// `<bytes>:<total>:<speed x1000>`, as the engine reports progress.
fn progress_payload(bytes: u64, total: u64, since: Instant) -> String {
    let secs = since.elapsed().as_secs_f64();
    let speed = if secs > 0.0 { bytes as f64 / secs } else { 0.0 };
    format!("{bytes}:{total}:{}", (speed * 1000.0) as i64)
}

/// Removes a half-written upload unless it was moved into place.
struct PartFile(Option<PathBuf>);

impl Drop for PartFile {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl Shared {
    fn token(&self) -> &str {
        &self.token
    }

    fn token_matches(&self, candidate: &str) -> bool {
        let (a, b) = (self.token.as_bytes(), candidate.as_bytes());
        a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }

    fn bump(&self) {
        self.version.send_modify(|v| *v += 1);
    }

    fn seen(&self, user_agent: &str) {
        let mut presence = self.presence.lock().unwrap();
        presence.last = Some(Instant::now());
        if !user_agent.is_empty() {
            presence.label = describe_browser(user_agent);
        }
    }

    /// (connected, "Safari on iPhone")
    fn presence(&self) -> (bool, String) {
        let presence = self.presence.lock().unwrap();
        let recent = presence.last.is_some_and(|t| t.elapsed() < CONNECTED_FOR);
        (presence.polling > 0 || recent, presence.label.clone())
    }

    fn with_batch<R>(&self, id: u64, f: impl FnOnce(&mut Batch) -> R) -> Option<R> {
        self.inner.lock().unwrap().batches.get_mut(&id).map(f)
    }

    // ------------------------------------------------------------ the page

    /// The page's content once its version differs from `since` (or `wait`
    /// passed). Texts it carries for the first time count as delivered.
    async fn state_after(self: &Arc<Self>, since: Option<u64>, wait: Duration) -> serde_json::Value {
        let mut rx = self.version.subscribe();
        if since == Some(*rx.borrow_and_update()) {
            // Counted while waiting, even if the browser goes away meanwhile
            // (which drops this future).
            struct Polling<'a>(&'a Mutex<Presence>);
            impl Drop for Polling<'_> {
                fn drop(&mut self) {
                    let mut presence = self.0.lock().unwrap();
                    presence.polling -= 1;
                    presence.last = Some(Instant::now());
                }
            }
            self.presence.lock().unwrap().polling += 1;
            let _polling = Polling(&self.presence);
            let _ = tokio::time::timeout(wait, rx.changed()).await;
        }
        let version = *self.version.borrow();
        let mut delivered = Vec::new();
        let items: Vec<serde_json::Value> = {
            let mut inner = self.inner.lock().unwrap();
            inner
                .offers
                .iter_mut()
                .rev()
                .map(|offer| match &offer.content {
                    Content::Text(text) => {
                        if !offer.done {
                            offer.done = true;
                            delivered.push((offer.key.clone(), offer.recorder.clone(), text.len()));
                        }
                        json!({"id": offer.id, "kind": "text", "text": text})
                    }
                    Content::Files(files) => json!({
                        "id": offer.id,
                        "kind": if files.single { "file" } else { "zip" },
                        "name": files.title,
                        "download": files.download_name,
                        "size": files.size,
                        "files": if files.loose {
                            files.entries.iter().map(|e| json!({"name": e.name, "size": e.size})).collect()
                        } else {
                            Vec::new()
                        },
                    }),
                })
                .collect()
        };
        for (key, recorder, len) in delivered {
            recorder.note("transfer-started", None);
            recorder.note(
                "transfer-completed",
                Some(&json!({"durationMs": 0, "bytes": len}).to_string()),
            );
            self.finish_row(key, "Sent to the browser".to_string());
        }
        json!({"v": version, "name": self.device_name, "items": items})
    }

    /// Drop a done send's row; its history entry takes over.
    fn finish_row(&self, key: String, msg: String) {
        let ctx = self.ctx.clone();
        let _ = self.ctx.weak.upgrade_in_event_loop(move |ui| {
            transfers::remove_row(&ui, &key);
            toast(&ui, &msg, false);
            refresh_history(&ctx);
        });
    }

    // ------------------------------------------------------- browser pulls

    async fn download(self: &Arc<Self>, offer_id: u64, file: Option<usize>) -> Reply {
        let files = {
            let inner = self.inner.lock().unwrap();
            inner.offers.iter().find(|o| o.id == offer_id).and_then(|o| match &o.content {
                Content::Files(files) => Some(files.clone()),
                Content::Text(_) => None,
            })
        };
        let Some(files) = files else {
            return http::error(StatusCode::NOT_FOUND, "No longer offered");
        };
        enum Source {
            File(PathBuf, u64),
            Zip(Vec<zip::Entry>),
        }
        let (name, len, source) = match file {
            None if files.single => {
                let entry = &files.entries[0];
                let path = entry.source.clone().expect("single offers are files");
                (files.download_name.clone(), entry.size, Source::File(path, entry.size))
            }
            None => (
                files.download_name.clone(),
                zip::archive_len(&files.entries),
                Source::Zip(files.entries.clone()),
            ),
            Some(i) if files.loose && i < files.entries.len() => {
                let entry = &files.entries[i];
                let path = entry.source.clone().expect("loose offers are files");
                (entry.name.clone(), entry.size, Source::File(path, entry.size))
            }
            Some(_) => return http::error(StatusCode::NOT_FOUND, "No such file"),
        };
        let (reply, tx) = http::download(&name, len);
        // Only whole downloads move the row's progress bar.
        let whole = file.is_none();
        let shared = self.clone();
        tokio::spawn(async move {
            let started = Instant::now();
            if whole {
                shared.offer_event(offer_id, "transfer-started", None);
            }
            let mut last = Instant::now();
            let progress = |sent: u64| {
                if whole && last.elapsed() >= PROGRESS_EVERY {
                    last = Instant::now();
                    let payload = progress_payload(sent, len, started);
                    shared.offer_event(offer_id, "transfer-progress", Some(payload));
                }
            };
            let result = match source {
                Source::File(path, size) => send_file(&path, size, &tx, progress).await,
                Source::Zip(entries) => zip::write(&entries, &tx, progress).await,
            };
            match result {
                Ok(()) => shared.downloaded(offer_id, file, files.size, started),
                Err(e) => {
                    tracing::debug!("browser link: download {offer_id} stopped: {e}");
                    let _ = tx.send(Err(e)).await;
                    if whole {
                        shared.offer_status(offer_id, "Download interrupted. The browser can try again.");
                    }
                }
            }
        });
        reply
    }

    /// Pass an event to an offer's history entry and row, while it has one.
    fn offer_event(&self, offer_id: u64, event: &'static str, payload: Option<String>) {
        let found = {
            let inner = self.inner.lock().unwrap();
            inner
                .offers
                .iter()
                .find(|o| o.id == offer_id && !o.done)
                .map(|o| (o.key.clone(), o.recorder.clone()))
        };
        let Some((key, recorder)) = found else { return };
        recorder.note(event, payload.as_deref());
        transfers::post_update(&self.ctx.weak, &key, move |row| {
            apply_transfer_event(row, event, payload.as_deref());
            if event == "transfer-started" {
                row.status = "Downloading in the browser…".into();
            }
        });
    }

    fn offer_status(&self, offer_id: u64, status: &'static str) {
        let key = {
            let inner = self.inner.lock().unwrap();
            inner
                .offers
                .iter()
                .find(|o| o.id == offer_id && !o.done)
                .map(|o| o.key.clone())
        };
        if let Some(key) = key {
            transfers::post_update(&self.ctx.weak, &key, move |row| {
                row.status = status.into();
                row.speed = "".into();
            });
        }
    }

    /// A download ran to its end: the whole offer, or one of its files.
    fn downloaded(&self, offer_id: u64, file: Option<usize>, bytes: u64, started: Instant) {
        let finished = {
            let mut inner = self.inner.lock().unwrap();
            let Some(offer) = inner.offers.iter_mut().find(|o| o.id == offer_id) else {
                return;
            };
            if offer.done {
                return;
            }
            let count = match &offer.content {
                Content::Files(files) => files.entries.len(),
                Content::Text(_) => 1,
            };
            if let Some(i) = file {
                offer.files_done.insert(i);
            }
            if file.is_some() && offer.files_done.len() < count {
                let (key, done) = (offer.key.clone(), offer.files_done.len());
                drop(inner);
                transfers::post_update(&self.ctx.weak, &key, move |row| {
                    row.status = format!("{done} of {count} files downloaded").into();
                });
                return;
            }
            offer.done = true;
            (offer.key.clone(), offer.recorder.clone())
        };
        let (key, recorder) = finished;
        recorder.note("transfer-started", None);
        recorder.note(
            "transfer-completed",
            Some(&json!({"durationMs": started.elapsed().as_millis() as u64, "bytes": bytes}).to_string()),
        );
        self.finish_row(key, "Sent to the browser".to_string());
    }

    // ------------------------------------------------------ browser pushes

    fn save_dir(&self) -> PathBuf {
        transfers::save_dir(&self.ctx, GUEST_NAME, GUEST_ID)
    }

    fn recorder(&self, ctx: Ctx, direction: TransferDirection) -> Arc<Recorder> {
        let history_enabled = self.ctx.settings.lock().unwrap().history_enabled;
        Arc::new(Recorder::new(
            self.ctx.history.clone(),
            direction,
            ctx,
            history_enabled,
        ))
    }

    async fn receive_text(&self, text: String) -> Reply {
        let save_dir = self.save_dir();
        let saved = async {
            tokio::fs::create_dir_all(&save_dir).await?;
            let target = free_path(save_dir.join(PASTE_FILE_NAME))?;
            tokio::fs::write(&target, &text).await
        }
        .await;
        if let Err(e) = saved {
            tracing::warn!("browser link: saving text failed: {e}");
            return http::error(StatusCode::INTERNAL_SERVER_ERROR, "Could not save the text");
        }
        let bytes = text.len() as u64;
        let recorder = self.recorder(
            Ctx {
                payload_bytes: bytes,
                item_count: 1,
                save_path: Some(save_dir.to_string_lossy().into_owned()),
                peer: Some(guest_peer()),
                ..Ctx::default()
            },
            TransferDirection::Receive,
        );
        recorder.note("receive-started", None);
        recorder.note("receive-file-names", Some(&json!([PASTE_FILE_NAME]).to_string()));
        recorder.note(
            "receive-completed",
            Some(&json!({"durationMs": 0, "bytes": bytes}).to_string()),
        );
        if bytes <= PASTE_MAX_BYTES {
            recorder.set_text(text);
        }
        let ctx = self.ctx.clone();
        let _ = self.ctx.weak.upgrade_in_event_loop(move |ui| {
            toast(&ui, "Text from the browser", false);
            refresh_history(&ctx);
        });
        http::json(&json!({}))
    }

    fn open_batch(self: &Arc<Self>, request: BatchRequest) -> Reply {
        let mut files = Vec::with_capacity(request.files.len());
        for file in request.files {
            let Some(name) = clean_relative(&file.name) else {
                return http::error(StatusCode::BAD_REQUEST, "Unusable file name");
            };
            files.push(Upload {
                name,
                size: file.size,
                done: false,
            });
        }
        let total: u64 = files.iter().map(|f| f.size).sum();
        let mut top_level: Vec<&str> = Vec::new();
        for file in &files {
            let head = file.name.split('/').next().unwrap_or_default();
            if !top_level.contains(&head) {
                top_level.push(head);
            }
        }
        let title = transfers::items_title(top_level[0], top_level.len());
        let save_dir = self.save_dir();
        let recorder = self.recorder(
            Ctx {
                // What a cancelled batch is listed as; completion fills in
                // the rest from the saved files.
                root_name: if top_level.len() == 1 {
                    top_level[0].to_string()
                } else {
                    String::new()
                },
                payload_bytes: total,
                item_count: top_level.len() as u32,
                save_path: Some(save_dir.to_string_lossy().into_owned()),
                peer: Some(guest_peer()),
                ..Ctx::default()
            },
            TransferDirection::Receive,
        );
        let key = self.ctx.transfers.new_key("recv");
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let id = {
            let mut inner = self.inner.lock().unwrap();
            if inner.closed {
                return http::error(StatusCode::GONE, "The link has ended");
            }
            inner.next_id += 1;
            let id = inner.next_id;
            let now = Instant::now();
            inner.batches.insert(
                id,
                Batch {
                    key: key.clone(),
                    files,
                    total,
                    received: 0,
                    save_dir,
                    recorder: recorder.clone(),
                    saved: Vec::new(),
                    conflicts: Vec::new(),
                    started: now,
                    last_activity: now,
                    cancelled: Arc::new(AtomicBool::new(false)),
                },
            );
            id
        };
        recorder.note("receive-started", None);
        self.ctx
            .transfers
            .recv_cancels
            .lock()
            .unwrap()
            .insert(key.clone(), cancel_tx);
        let _ = self.ctx.weak.upgrade_in_event_loop(move |ui| {
            transfers::insert_row(
                &ui,
                TransferRow {
                    key: key.into(),
                    peer_id: GUEST_ID.into(),
                    sending: false,
                    active: true,
                    title: title.into(),
                    progress_label: fmt_bytes(total).into(),
                    status: "Receiving from the browser…".into(),
                    ..Default::default()
                },
            );
            toast(&ui, "The browser is sending you files", false);
        });
        tokio::spawn(self.clone().watch_batch(id, cancel_rx));
        http::json(&json!({"id": id}))
    }

    /// Ends a batch the user cancelled, or one the browser went quiet on.
    async fn watch_batch(self: Arc<Self>, id: u64, mut cancel: oneshot::Receiver<()>) {
        loop {
            match tokio::time::timeout(Duration::from_secs(5), &mut cancel).await {
                Ok(Ok(())) => return self.end_batch(id, Ended::CancelledHere),
                // Finished (or ended) elsewhere, which drops the sender.
                Ok(Err(_)) => return,
                Err(_) => {
                    let stalled = self.with_batch(id, |b| b.last_activity.elapsed() > BATCH_IDLE);
                    match stalled {
                        None => return,
                        Some(true) => return self.end_batch(id, Ended::Stalled),
                        Some(false) => {}
                    }
                }
            }
        }
    }

    fn browser_cancelled(&self, id: u64) {
        self.end_batch(id, Ended::ByBrowser);
    }

    fn end_batch(&self, id: u64, why: Ended) {
        let Some(batch) = self.inner.lock().unwrap().batches.remove(&id) else {
            return;
        };
        end_batch(&self.ctx, batch, why);
    }

    async fn upload(&self, batch_id: u64, index: usize, mut body: Incoming) -> Reply {
        let gone = || http::error(StatusCode::GONE, "This upload was cancelled");
        let found = self.with_batch(batch_id, |b| {
            b.last_activity = Instant::now();
            let file = b.files.get(index)?;
            Some((
                file.name.clone(),
                file.size,
                file.done,
                b.save_dir.clone(),
                b.cancelled.clone(),
                b.key.clone(),
                b.received,
                b.total,
                b.started,
            ))
        });
        let Some(found) = found else { return gone() };
        let Some((name, size, done, save_dir, cancelled, key, received, total, started)) = found
        else {
            return http::error(StatusCode::NOT_FOUND, "No such file");
        };
        if done {
            return http::json(&json!({}));
        }
        let target = name.split('/').fold(save_dir, |path, part| path.join(part));
        let file_name = path_file_name(&target);
        let part_path = target.with_file_name(format!(".{file_name}.{batch_id}-{index}.flipflop-part"));
        let mut part = PartFile(Some(part_path.clone()));
        let failed = |e: io::Error| {
            tracing::warn!("browser link: saving {name} failed: {e}");
            http::error(StatusCode::INTERNAL_SERVER_ERROR, "Could not save the file")
        };
        if let Some(parent) = target.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return failed(e);
            }
        }
        let mut file = match tokio::fs::File::create(&part_path).await {
            Ok(file) => file,
            Err(e) => return failed(e),
        };
        let mut written = 0u64;
        let mut last = Instant::now();
        while let Some(chunk) = http::next_chunk(&mut body).await {
            let Ok(chunk) = chunk else {
                return http::error(StatusCode::BAD_REQUEST, "Upload interrupted");
            };
            if cancelled.load(Ordering::SeqCst) {
                return gone();
            }
            written += chunk.len() as u64;
            if written > size {
                return http::error(StatusCode::BAD_REQUEST, "More data than announced");
            }
            if let Err(e) = file.write_all(&chunk).await {
                return failed(e);
            }
            if last.elapsed() >= PROGRESS_EVERY {
                last = Instant::now();
                self.with_batch(batch_id, |b| b.last_activity = last);
                let payload = progress_payload(received + written, total, started);
                transfers::post_update(&self.ctx.weak, &key, move |row| {
                    apply_transfer_event(row, "receive-progress", Some(&payload));
                });
            }
        }
        if written != size {
            return http::error(StatusCode::BAD_REQUEST, "Upload incomplete");
        }
        if let Err(e) = file.flush().await {
            return failed(e);
        }
        drop(file);
        if cancelled.load(Ordering::SeqCst) {
            return gone();
        }
        let dest = match free_path(target.clone()) {
            Ok(dest) => dest,
            Err(e) => return failed(e),
        };
        if let Err(e) = tokio::fs::rename(&part_path, &dest).await {
            return failed(e);
        }
        part.0 = None;
        self.file_saved(batch_id, index, &target, &dest);
        http::json(&json!({}))
    }

    fn file_saved(&self, batch_id: u64, index: usize, wanted: &Path, saved: &Path) {
        let finished = {
            let mut inner = self.inner.lock().unwrap();
            let Some(batch) = inner.batches.get_mut(&batch_id) else {
                return;
            };
            let file = &mut batch.files[index];
            file.done = true;
            batch.received += file.size;
            batch.saved.push(file.name.clone());
            if wanted != saved {
                batch.conflicts.push(json!({
                    "original": wanted.to_string_lossy(),
                    "resolved": saved.to_string_lossy(),
                }));
            }
            batch.last_activity = Instant::now();
            if batch.files.iter().all(|f| f.done) {
                inner.batches.remove(&batch_id)
            } else {
                let payload = progress_payload(batch.received, batch.total, batch.started);
                let key = batch.key.clone();
                drop(inner);
                transfers::post_update(&self.ctx.weak, &key, move |row| {
                    apply_transfer_event(row, "receive-progress", Some(&payload));
                });
                return;
            }
        };
        let Some(batch) = finished else { return };
        // Dropping the cancel sender lets the batch's watcher go.
        self.ctx.transfers.recv_cancels.lock().unwrap().remove(&batch.key);
        let recorder = &batch.recorder;
        recorder.note("receive-file-names", Some(&json!(batch.saved).to_string()));
        if !batch.conflicts.is_empty() {
            recorder.note("receive-conflicts", Some(&json!(batch.conflicts).to_string()));
        }
        recorder.note(
            "receive-completed",
            Some(
                &json!({
                    "durationMs": batch.started.elapsed().as_millis() as u64,
                    "bytes": batch.total,
                    "outputDir": batch.save_dir.to_string_lossy(),
                })
                .to_string(),
            ),
        );
        self.finish_row(batch.key, "Files from the browser saved".to_string());
    }
}

/// Settle a batch that won't complete: its row, history entry and upload.
fn end_batch(ctx: &AppCtx, batch: Batch, why: Ended) {
    batch.cancelled.store(true, Ordering::SeqCst);
    ctx.transfers.recv_cancels.lock().unwrap().remove(&batch.key);
    let (status, error) = match why {
        Ended::Stalled => (
            TransferStatus::Failed,
            Some("The browser stopped sending".to_string()),
        ),
        _ => (TransferStatus::Cancelled, None),
    };
    // Files saved so far are listed in the entry.
    if !batch.saved.is_empty() {
        batch
            .recorder
            .note("receive-file-names", Some(&json!(batch.saved).to_string()));
    }
    batch.recorder.finalize(status, None, None, None, error.clone());
    let key = batch.key;
    let ctx_ui = ctx.clone();
    let _ = ctx.weak.upgrade_in_event_loop(move |ui| {
        match (why, error) {
            (_, Some(msg)) => {
                transfers::update_row(&ui, &key, |row| {
                    row.active = false;
                    row.status = "Failed".into();
                    row.speed = "".into();
                    row.error = msg.clone().into();
                });
                toast(&ui, &msg, true);
            }
            (Ended::LinkClosed, None) => transfers::remove_row(&ui, &key),
            (Ended::ByBrowser, None) => {
                transfers::remove_row(&ui, &key);
                toast(&ui, "The browser stopped sending", false);
            }
            (_, None) => {
                transfers::remove_row(&ui, &key);
                toast(&ui, "Receive cancelled", false);
            }
        }
        refresh_history(&ctx_ui);
    });
}

async fn send_file(
    path: &Path,
    size: u64,
    tx: &mpsc::Sender<io::Result<Bytes>>,
    mut sent: impl FnMut(u64),
) -> io::Result<()> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut done = 0u64;
    while done < size {
        let mut buf = vec![0u8; CHUNK.min((size - done) as usize)];
        let n = file.read(&mut buf).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("{} got shorter", path.display()),
            ));
        }
        buf.truncate(n);
        done += n as u64;
        tx.send(Ok(buf.into()))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "download closed"))?;
        sent(done);
    }
    Ok(())
}

// ------------------------------------------------------------- file names

fn path_file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `path`, or the first free "name (n).ext" next to it.
fn free_path(path: PathBuf) -> io::Result<PathBuf> {
    if !path.exists() {
        return Ok(path);
    }
    crate::engine::export::resolve_conflict_path(&path).map_err(io::Error::other)
}

/// A browser-supplied relative path made safe to join onto the save dir:
/// every part cleaned of separators and characters Windows rejects, with
/// `.`/`..` and empty parts dropped. `None` if nothing usable is left.
fn clean_relative(name: &str) -> Option<String> {
    const FORBIDDEN: &[char] = &['\\', ':', '*', '?', '"', '<', '>', '|'];
    let parts: Vec<String> = name
        .split('/')
        .map(|part| {
            let cleaned: String = part
                .chars()
                .map(|c| {
                    if c.is_control() || FORBIDDEN.contains(&c) {
                        '_'
                    } else {
                        c
                    }
                })
                .collect();
            cleaned
                .trim_matches(|c: char| c == '.' || c.is_whitespace())
                .chars()
                .take(200)
                .collect::<String>()
        })
        .filter(|part| !part.is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

// ------------------------------------------------------------- offering

/// What `paths` look like on the page. Blocking (walks folders).
fn scan(paths: &[PathBuf]) -> io::Result<Files> {
    let mut entries: Vec<zip::Entry> = Vec::new();
    let mut taken: HashSet<String> = HashSet::new();
    for path in paths {
        let md = std::fs::metadata(path)?;
        // Two picked files may share a name; the archive needs distinct ones.
        let mut top = transfers::path_name(path);
        let mut n = 1;
        while !taken.insert(top.clone()) {
            top = format!("{} ({n})", transfers::path_name(path));
            n += 1;
        }
        if !md.is_dir() {
            entries.push(zip::Entry {
                name: top,
                source: Some(path.clone()),
                size: md.len(),
                dos_time: zip::dos_time(md.modified().ok()),
            });
            continue;
        }
        entries.push(zip::Entry {
            name: format!("{top}/"),
            source: None,
            size: 0,
            dos_time: zip::dos_time(md.modified().ok()),
        });
        for entry in walkdir::WalkDir::new(path)
            .min_depth(1)
            .sort_by_file_name()
        {
            let entry = entry?;
            let rel = entry.path().strip_prefix(path).unwrap_or(entry.path());
            let rel: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            let name = format!("{top}/{}", rel.join("/"));
            let md = entry.metadata()?;
            if entry.file_type().is_dir() {
                entries.push(zip::Entry {
                    name: format!("{name}/"),
                    source: None,
                    size: 0,
                    dos_time: zip::dos_time(md.modified().ok()),
                });
            } else if entry.file_type().is_file() {
                entries.push(zip::Entry {
                    name,
                    source: Some(entry.path().to_path_buf()),
                    size: md.len(),
                    dos_time: zip::dos_time(md.modified().ok()),
                });
            }
        }
    }
    let size = entries.iter().map(|e| e.size).sum();
    let all_files = entries.iter().all(|e| e.source.is_some());
    let single = entries.len() == 1 && all_files;
    let first = transfers::path_name(&paths[0]);
    let title = transfers::items_title(&first, paths.len());
    let download_name = if single {
        entries[0].name.clone()
    } else if paths.len() == 1 {
        format!("{first}.zip")
    } else {
        "flipflop files.zip".to_string()
    };
    Ok(Files {
        title,
        download_name,
        entries,
        size,
        single,
        loose: !single && all_files,
    })
}

/// Put `paths` (or a pasted text: one paste file, with its private `dir`)
/// on the page. UI thread; `staged` share-sheet files are claimed by the
/// caller's row until the link ends.
pub(crate) fn offer(
    ctx: &AppCtx,
    ui: &AppWindow,
    paths: Vec<PathBuf>,
    staged: Vec<PathBuf>,
    paste_dir: Option<PathBuf>,
) {
    let Some(shared) = ctx.guest.shared() else {
        toast(ui, "The browser link has ended", true);
        return;
    };
    let key = ctx.transfers.new_key("send");
    let first_name = paths.first().map(|p| transfers::path_name(p)).unwrap_or_default();
    transfers::insert_row(
        ui,
        TransferRow {
            key: key.clone().into(),
            peer_id: GUEST_ID.into(),
            sending: true,
            active: true,
            title: transfers::items_title(&first_name, paths.len()).into(),
            status: "Preparing…".into(),
            ..Default::default()
        },
    );
    transfers::claim(ctx, &staged);
    let ctx = ctx.clone();
    ctx.rt.clone().spawn_blocking(move || {
        // A paste is shown as text on the page, however long.
        let content = match paste_dir {
            Some(dir) => {
                let text = std::fs::read_to_string(&paths[0]).map(Content::Text);
                let _ = std::fs::remove_dir_all(dir);
                text
            }
            None => match transfers::sent_paste_text(&paths) {
                Some(text) => Ok(Content::Text(text)),
                None => scan(&paths).map(Content::Files),
            },
        };
        let content = match content {
            Ok(content) => content,
            Err(e) => {
                transfers::release(&ctx, &staged);
                return transfers::fail(&ctx.weak, &key, format!("Could not read the files: {e}"));
            }
        };
        let (root_name, bytes, item_count, path_type, text) = match &content {
            Content::Text(text) => (
                PASTE_FILE_NAME.to_string(),
                text.len() as u64,
                1,
                Some(TransferPathType::File),
                Some(text.clone()),
            ),
            Content::Files(files) => (
                first_name.clone(),
                files.size,
                paths.len() as u32,
                match paths.as_slice() {
                    [only] if only.is_dir() => Some(TransferPathType::Directory),
                    [_] => Some(TransferPathType::File),
                    _ => None,
                },
                None,
            ),
        };
        let recorder = shared.recorder(
            Ctx {
                root_name,
                payload_bytes: bytes,
                item_count,
                path_type,
                peer: Some(guest_peer()),
                text,
                ..Ctx::default()
            },
            TransferDirection::Send,
        );
        let is_text = matches!(content, Content::Text(_));
        {
            let mut inner = shared.inner.lock().unwrap();
            if inner.closed || inner.withdrawn.remove(&key) {
                drop(inner);
                transfers::release(&ctx, &staged);
                let _ = ctx.weak.upgrade_in_event_loop(move |ui| transfers::remove_row(&ui, &key));
                return;
            }
            inner.next_id += 1;
            let id = inner.next_id;
            inner.offers.push(Offer {
                id,
                key: key.clone(),
                content,
                recorder,
                staged,
                done: false,
                files_done: HashSet::new(),
            });
        }
        transfers::post_update(&ctx.weak, &key, move |row| {
            row.progress_label = fmt_bytes(bytes).into();
            row.status = if is_text {
                "Waiting for the browser to open the link…"
            } else {
                "On the link's page, waiting for a download"
            }
            .into();
        });
        shared.bump();
    });
}

/// Stop on a send to the browser: take it off the page. UI thread.
pub(crate) fn withdraw(ctx: &AppCtx, key: &str) {
    if let Some(shared) = ctx.guest.shared() {
        let offer = {
            let mut inner = shared.inner.lock().unwrap();
            match inner.offers.iter().position(|o| o.key == key) {
                Some(i) => Some(inner.offers.remove(i)),
                None => {
                    // Still being prepared.
                    inner.withdrawn.insert(key.to_string());
                    None
                }
            }
        };
        if let Some(offer) = offer {
            offer
                .recorder
                .finalize(TransferStatus::Cancelled, None, None, None, None);
            transfers::release(ctx, &offer.staged);
            shared.bump();
        }
    }
    if let Some(ui) = ctx.weak.upgrade() {
        transfers::remove_row(&ui, key);
        toast(&ui, "Send stopped", false);
    }
    refresh_history(ctx);
}

// ------------------------------------------------------------ the session

fn random_token() -> String {
    let mut bytes = [0u8; 12];
    getrandom::fill(&mut bytes).expect("system randomness");
    data_encoding::BASE32_NOPAD
        .encode(&bytes)
        .to_ascii_lowercase()
}

/// Interface names that are never the network a guest is on: mobile data,
/// VPN tunnels and virtual bridges.
fn unlikely_interface(name: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "rmnet", "ccmni", "v4-", "clat", "pdp", "wwan", "dummy", "docker", "br-", "veth", "virbr",
        "vmnet", "vboxnet", "tun", "utun", "wg", "tailscale", "zt",
    ];
    let name = name.to_ascii_lowercase();
    PREFIXES.iter().any(|p| name.starts_with(p))
}

/// Our IPv4 addresses a browser nearby could reach, likeliest first.
async fn local_addresses() -> Vec<Ipv4Addr> {
    let state = netwatch::interfaces::State::new().await;
    // The address the OS would reach the internet from.
    let routed = std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|socket| {
            socket.connect("192.0.2.1:9")?;
            socket.local_addr()
        })
        .ok()
        .and_then(|addr| match addr.ip() {
            IpAddr::V4(ip) => Some(ip),
            IpAddr::V6(_) => None,
        });
    let mut found: Vec<(u8, Ipv4Addr)> = Vec::new();
    for (name, iface) in &state.interfaces {
        if !iface.is_up() {
            continue;
        }
        for net in iface.addrs() {
            let IpAddr::V4(ip) = net.addr() else { continue };
            if ip.is_loopback() || ip.is_link_local() || ip.is_unspecified() || ip.is_multicast() {
                continue;
            }
            let rank = if unlikely_interface(name) {
                3
            } else if Some(ip) == routed || state.default_route_interface.as_ref() == Some(name) {
                0
            } else if ip.is_private() {
                1
            } else {
                2
            };
            found.push((rank, ip));
        }
    }
    if found.is_empty() {
        found.extend(routed.map(|ip| (0, ip)));
    }
    found.sort();
    let mut addresses: Vec<Ipv4Addr> = found.into_iter().map(|(_, ip)| ip).collect();
    addresses.dedup();
    addresses
}

/// "Safari on iPhone", from a User-Agent header.
fn describe_browser(ua: &str) -> String {
    let device = [
        ("iPhone", "iPhone"),
        ("iPad", "iPad"),
        ("Android", "Android"),
        ("CrOS", "Chromebook"),
        ("Windows", "Windows"),
        ("Macintosh", "Mac"),
        ("Linux", "Linux"),
    ]
    .into_iter()
    .find(|(needle, _)| ua.contains(needle))
    .map(|(_, name)| name);
    let browser = [
        ("Edg", "Edge"),
        ("OPR/", "Opera"),
        ("SamsungBrowser", "Samsung Internet"),
        ("Firefox", "Firefox"),
        ("FxiOS", "Firefox"),
        ("CriOS", "Chrome"),
        ("Chrome", "Chrome"),
        ("Safari", "Safari"),
    ]
    .into_iter()
    .find(|(needle, _)| ua.contains(needle))
    .map(|(_, name)| name);
    match (browser, device) {
        (Some(b), Some(d)) => format!("{b} on {d}"),
        (Some(b), None) => b.to_string(),
        (None, Some(d)) => d.to_string(),
        (None, None) => "A browser".to_string(),
    }
}

/// The guest's row in the peer list, while a link is open.
pub(crate) fn peer_row(ctx: &AppCtx) -> Option<PeerRow> {
    let shared = ctx.guest.shared()?;
    let (online, label) = shared.presence();
    Some(PeerRow {
        endpoint_id: GUEST_ID.into(),
        name: GUEST_NAME.into(),
        initials: "".into(),
        detail: if online {
            label
        } else {
            "Waiting for a browser to open the link".to_string()
        }
        .into(),
        online,
        guest: true,
        ..Default::default()
    })
}

/// Refresh the guest's row (and page header) in place. UI thread.
fn sync_row(ctx: &AppCtx, ui: &AppWindow) {
    let Some(row) = peer_row(ctx) else { return };
    let state = ui.global::<State>();
    let peers = state.get_peers();
    for i in 0..peers.row_count() {
        if let Some(old) = peers.row_data(i).filter(|p| p.guest) {
            let row = PeerRow {
                sending: old.sending,
                receiving: old.receiving,
                ..row.clone()
            };
            peers.set_row_data(i, row);
        }
    }
    if is_guest(&state.get_selected_id()) {
        show_selected(&state, Some(&row));
    }
}

/// Mirror the link (URL, QR code) into the UI. UI thread.
fn show_link(ctx: &AppCtx, ui: &AppWindow, session: Option<&Session>) {
    let state = ui.global::<State>();
    let url = session.and_then(|s| s.url(ctx)).unwrap_or_default();
    if url != state.get_guest_url().as_str() {
        if let Some(buf) = (!url.is_empty()).then(|| crate::qr::render(&url)).flatten() {
            state.set_guest_qr(slint::Image::from_rgb8(buf));
        }
        state.set_guest_url(SharedString::from(url));
    }
    state.set_guest_active(session.is_some());
    state.set_guest_web(session.is_some_and(|s| s.web));
    state.set_guest_more_addresses(session.is_some_and(|s| !s.web && s.addresses.len() > 1));
}

/// Open the guest's page. UI thread.
fn open(ui: &AppWindow) {
    let state = ui.global::<State>();
    state.set_rename_mode(false);
    state.set_selected_id(GUEST_ID.into());
    state.set_page("peer".into());
    state.set_peer_open(true);
    ui.global::<Logic>().invoke_page_changed("peer".into());
}

async fn launch(ctx: &AppCtx) -> io::Result<Session> {
    let listener = match tokio::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, PORT)).await {
        Ok(listener) => listener,
        Err(_) => tokio::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await?,
    };
    let port = listener.local_addr()?.port();
    let addresses = local_addresses().await;
    let device_name = ctx
        .node()
        .map(|node| node.device_info().display_name)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "flipflop".to_string());
    let shared = Arc::new(Shared {
        ctx: ctx.clone(),
        token: random_token(),
        device_name,
        inner: Mutex::new(Inner::default()),
        version: watch::Sender::new(1),
        presence: Mutex::new(Presence::default()),
    });
    tracing::info!(port, ?addresses, "browser link: listening");
    let server = tokio::spawn(http::serve(listener, shared.clone()));
    let tunnel = ctx.node().map(|node| {
        let mut inbox = node.guest_inbox().listen();
        let shared = shared.clone();
        tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    conn = inbox.recv() => {
                        let Some(conn) = conn else { break };
                        tracing::info!(remote = %conn.remote_id().fmt_short(), "browser link: web page connected");
                        connections.spawn(http::serve_tunnel(conn, shared.clone()));
                    }
                    Some(_) = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        })
    });
    let presence = tokio::spawn(watch_presence(shared.clone()));
    Ok(Session {
        shared,
        port,
        // Nobody on our network to share with: offer the web link.
        web: addresses.is_empty(),
        addresses,
        address: 0,
        _server: AbortOnDropHandle::new(server),
        _tunnel: tunnel.map(AbortOnDropHandle::new),
        _presence: AbortOnDropHandle::new(presence),
    })
}

/// Keep the guest's online dot in step with the browser.
async fn watch_presence(shared: Arc<Shared>) {
    let mut connected = false;
    let mut ticks = tokio::time::interval(Duration::from_secs(2));
    loop {
        ticks.tick().await;
        let (now, _) = shared.presence();
        if now == connected {
            continue;
        }
        connected = now;
        let ctx = shared.ctx.clone();
        let _ = shared.ctx.weak.upgrade_in_event_loop(move |ui| {
            sync_row(&ctx, &ui);
            if connected {
                toast(&ui, "A browser opened the link", false);
            }
        });
    }
}

/// Open a link (or show the open one). UI thread.
pub(crate) fn start(ctx: &AppCtx) {
    if ctx.guest.is_active() {
        if let Some(ui) = ctx.weak.upgrade() {
            open(&ui);
        }
        return;
    }
    if ctx.guest.starting.swap(true, Ordering::SeqCst) {
        return;
    }
    let ctx = ctx.clone();
    ctx.rt.clone().spawn(async move {
        let result = launch(&ctx).await;
        ctx.guest.starting.store(false, Ordering::SeqCst);
        let ctx_ui = ctx.clone();
        let _ = ctx.weak.upgrade_in_event_loop(move |ui| match result {
            Ok(session) => {
                let mut slot = ctx_ui.guest.session.lock().unwrap();
                show_link(&ctx_ui, &ui, Some(&session));
                *slot = Some(session);
                drop(slot);
                refresh_peers(&ctx_ui);
                open(&ui);
            }
            Err(e) => {
                tracing::warn!("browser link: could not start: {e}");
                toast(&ui, &format!("Could not open a browser link: {e}"), true);
            }
        });
    });
}

/// End the link: stop the server, withdraw what's still offered, drop
/// unfinished uploads. UI thread.
pub(crate) fn stop(ctx: &AppCtx) {
    let Some(session) = ctx.guest.session.lock().unwrap().take() else {
        return;
    };
    let shared = session.shared.clone();
    // Stops the server and every connection with it.
    drop(session);
    let (offers, batches) = {
        let mut inner = shared.inner.lock().unwrap();
        inner.closed = true;
        (
            std::mem::take(&mut inner.offers),
            std::mem::take(&mut inner.batches),
        )
    };
    let ui = ctx.weak.upgrade();
    for offer in offers {
        if !offer.done {
            offer
                .recorder
                .finalize(TransferStatus::Cancelled, None, None, None, None);
            if let Some(ui) = &ui {
                transfers::remove_row(ui, &offer.key);
            }
        } else if !offer.staged.is_empty() {
            android::clear_outbox(&offer.staged);
        }
        transfers::release(ctx, &offer.staged);
    }
    for (_, batch) in batches {
        end_batch(ctx, batch, Ended::LinkClosed);
    }
    if let Some(ui) = &ui {
        show_link(ctx, ui, None);
        toast(ui, "Browser link closed", false);
    }
    refresh_peers(ctx);
    refresh_history(ctx);
}

/// Re-read our addresses after a network change. Any thread.
pub(crate) fn refresh_addresses(ctx: &AppCtx) {
    if !ctx.guest.is_active() {
        return;
    }
    let ctx = ctx.clone();
    ctx.rt.clone().spawn(async move {
        let addresses = local_addresses().await;
        let ctx_ui = ctx.clone();
        let _ = ctx.weak.upgrade_in_event_loop(move |ui| {
            let mut slot = ctx_ui.guest.session.lock().unwrap();
            let Some(session) = slot.as_mut() else { return };
            let shown = session.addresses.get(session.address).copied();
            session.address = shown
                .and_then(|ip| addresses.iter().position(|a| *a == ip))
                .unwrap_or(0);
            session.addresses = addresses;
            show_link(&ctx_ui, &ui, Some(session));
        });
    });
}

pub(crate) fn register(ctx: &AppCtx) {
    let Some(ui) = ctx.weak.upgrade() else {
        return;
    };
    let logic = ui.global::<Logic>();
    {
        let ctx = ctx.clone();
        logic.on_guest_start(move || start(&ctx));
    }
    {
        let ctx = ctx.clone();
        logic.on_guest_stop(move || stop(&ctx));
    }
    {
        let ctx = ctx.clone();
        logic.on_guest_copy_url(move || {
            let Some(ui) = ctx.weak.upgrade() else { return };
            let url = ui.global::<State>().get_guest_url().to_string();
            if url.is_empty() {
                return;
            }
            match copy_to_clipboard(&url) {
                Ok(()) => toast(&ui, "Link copied", false),
                Err(e) => toast(&ui, &format!("Copy failed: {e}"), true),
            }
        });
    }
    {
        let ctx = ctx.clone();
        logic.on_guest_next_address(move || {
            let Some(ui) = ctx.weak.upgrade() else { return };
            let mut slot = ctx.guest.session.lock().unwrap();
            let Some(session) = slot.as_mut() else { return };
            if !session.addresses.is_empty() {
                session.address = (session.address + 1) % session.addresses.len();
            }
            show_link(&ctx, &ui, Some(session));
        });
    }
    {
        let ctx = ctx.clone();
        logic.on_guest_set_web(move |web| {
            let Some(ui) = ctx.weak.upgrade() else { return };
            let mut slot = ctx.guest.session.lock().unwrap();
            let Some(session) = slot.as_mut() else { return };
            session.web = web;
            show_link(&ctx, &ui, Some(session));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_names_cannot_escape() {
        assert_eq!(clean_relative("a.txt").as_deref(), Some("a.txt"));
        assert_eq!(clean_relative("photos/2024/a.jpg").as_deref(), Some("photos/2024/a.jpg"));
        assert_eq!(clean_relative("../../etc/passwd").as_deref(), Some("etc/passwd"));
        assert_eq!(clean_relative("/abs/./x").as_deref(), Some("abs/x"));
        assert_eq!(clean_relative("C:\\Windows\\x").as_deref(), Some("C__Windows_x"));
        assert_eq!(clean_relative("a\u{0}b").as_deref(), Some("a_b"));
        assert_eq!(clean_relative("../.."), None);
        assert_eq!(clean_relative(""), None);
    }

    #[test]
    fn browsers_are_named() {
        let iphone = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1";
        assert_eq!(describe_browser(iphone), "Safari on iPhone");
        let android = "Mozilla/5.0 (Linux; Android 14) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Mobile Safari/537.36";
        assert_eq!(describe_browser(android), "Chrome on Android");
        let edge = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36 Edg/120.0";
        assert_eq!(describe_browser(edge), "Edge on Windows");
        assert_eq!(describe_browser("curl/8.0"), "A browser");
    }

    #[test]
    fn web_links_carry_everything_in_the_fragment() {
        let url = web_url("abc", "ae58ff", "https://relay.example/");
        assert_eq!(
            url,
            format!("{WEB_PAGE}#t=abc&e=ae58ff&r=https%3A%2F%2Frelay.example%2F")
        );
    }

    #[test]
    fn tokens_are_long_and_url_safe() {
        let token = random_token();
        assert_eq!(token.len(), 20);
        assert!(token.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
        assert_ne!(token, random_token());
    }

    #[test]
    fn mobile_and_virtual_interfaces_rank_last() {
        assert!(unlikely_interface("rmnet_data0"));
        assert!(unlikely_interface("docker0"));
        assert!(!unlikely_interface("wlan0"));
        assert!(!unlikely_interface("en0"));
        assert!(!unlikely_interface("ap0"));
    }

    #[test]
    fn scanned_offers_pick_single_loose_or_zip() {
        let dir = std::env::temp_dir().join(format!("tm-guest-scan-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("folder/sub")).unwrap();
        std::fs::write(dir.join("a.txt"), b"aa").unwrap();
        std::fs::write(dir.join("b.txt"), b"bbb").unwrap();
        std::fs::write(dir.join("folder/sub/c.txt"), b"c").unwrap();

        let one = scan(&[dir.join("a.txt")]).unwrap();
        assert!(one.single && !one.loose);
        assert_eq!((one.download_name.as_str(), one.size), ("a.txt", 2));

        let two = scan(&[dir.join("a.txt"), dir.join("b.txt")]).unwrap();
        assert!(!two.single && two.loose);
        assert_eq!((two.title.as_str(), two.size), ("2 items", 5));

        let folder = scan(&[dir.join("folder")]).unwrap();
        assert!(!folder.single && !folder.loose);
        assert_eq!(folder.download_name, "folder.zip");
        let names: Vec<&str> = folder.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["folder/", "folder/sub/", "folder/sub/c.txt"]);

        let same = scan(&[dir.join("a.txt"), dir.join("a.txt")]).unwrap();
        assert_eq!(same.entries[1].name, "a.txt (1)");
        let _ = std::fs::remove_dir_all(dir);
    }
}
