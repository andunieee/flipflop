//! Send and receive flows.
//!
//! Every transfer gets a unique key and a `TransferRow` in `State.transfers`;
//! each peer page shows the rows for that peer, so sends to and receives from
//! different peers run (and are shown) independently. Rows are only touched
//! on the UI thread; background tasks post updates with `post_update`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::engine::{
    sanitize_folder_name, AppHandle, EventEmitter, NodeService, NodeShare, PairedDeviceInfo,
    TransferDirection, TransferPeer, TransferStatus,
};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel, Weak};

use crate::app::{refresh_history, short_id, AppCtx};
use crate::emitter::apply_transfer_event;
use crate::format::fmt_bytes;
use crate::platform::{self, toast, toast_later};
use crate::recorder::{Ctx, Recorder};
use crate::{android, guest, AppWindow, Logic, OutboxRow, QuickOffer, State, TransferRow};

/// Cross-thread transfer bookkeeping, shared through `AppCtx`.
#[derive(Clone, Default)]
pub struct Transfers {
    shares: Arc<tokio::sync::Mutex<Shares>>,
    /// Cancel senders of running receives, by key. A receive whose sender is
    /// gone when it ends was cancelled by the user.
    pub(crate) recv_cancels: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>>,
    next_key: Arc<AtomicU64>,
    /// A file picker is open (ignore further "Files" clicks).
    #[cfg_attr(target_os = "android", allow(dead_code))]
    picking: Arc<AtomicBool>,
    /// Share-sheet files a send has taken from the queue; they leave the
    /// queue for good once that send completes, or return if it does not.
    claimed: Arc<Mutex<HashSet<PathBuf>>>,
    /// Sends not yet delivered to an unreachable peer, by row key; saved to
    /// `queue_path` so they survive restarts.
    queued: Arc<Mutex<HashMap<String, QueuedSend>>>,
    /// Shared sends whose invite is waiting for the peer, by row key.
    waiting: Arc<Mutex<HashMap<String, PendingInvite>>>,
    /// Queued sends restored at startup whose share isn't set up yet.
    restored: Arc<Mutex<HashSet<String>>>,
    queue_path: Arc<OnceLock<PathBuf>>,
    /// Desktop: files picked for the composer and not sent yet (Android
    /// stages copies in its outbox instead).
    picked: Arc<Mutex<Vec<PathBuf>>>,
    /// Quick shares offered to us, waiting for Accept or Decline.
    incoming: Arc<Mutex<Vec<IncomingQuick>>>,
}

/// A quick share someone nearby offered us.
#[derive(Clone)]
struct IncomingQuick {
    /// Our handle on it (the UI's `QuickOffer.key`).
    key: String,
    /// The sender's, echoed in a decline.
    id: String,
    title: String,
    invite: Invite,
}

/// One send the composer produced: files, or a pasted text's file in its
/// private dir (`cleanup`). `staged` are the share-sheet files among `paths`.
pub(crate) struct Outgoing {
    pub paths: Vec<PathBuf>,
    pub staged: Vec<PathBuf>,
    pub cleanup: Option<PathBuf>,
}

impl Outgoing {
    /// Drop a send that won't happen.
    pub(crate) fn discard(self, ctx: &AppCtx) {
        if let Some(dir) = self.cleanup {
            let _ = std::fs::remove_dir_all(dir);
        }
        refresh_outbox(ctx);
    }
}

/// A send waiting for its peer, as saved to disk.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct QueuedSend {
    peer_id: String,
    peer_name: String,
    /// What is shared; empty for a pasted text.
    #[serde(default)]
    paths: Vec<PathBuf>,
    /// Share-sheet (outbox) files among `paths`.
    #[serde(default)]
    staged: Vec<PathBuf>,
    /// A pasted text, written to a fresh paste file when the send restarts.
    #[serde(default)]
    text: Option<String>,
}

/// The invite of an open share that could not reach its peer yet.
struct PendingInvite {
    peer_id: String,
    peer_name: String,
    ticket: String,
    item_count: u32,
    byte_count: u64,
    /// A delivery attempt is in flight.
    attempting: bool,
}

#[derive(Default)]
struct Shares {
    open: HashMap<String, ShareHandle>,
    /// Sends stopped while their share was still being set up; the setup
    /// task shuts the share down instead of registering it.
    stopped: HashSet<String>,
}

impl Transfers {
    pub(crate) fn new_key(&self, prefix: &str) -> String {
        format!("{prefix}-{}", self.next_key.fetch_add(1, Ordering::Relaxed))
    }
}

struct ShareHandle {
    /// Served from the node's endpoint until this handle drops.
    _share: NodeShare,
    recorder: Arc<Recorder>,
    peer_name: String,
    /// Android share-sheet files this share serves; deleted once no open
    /// share needs them any more.
    staged: Vec<PathBuf>,
    /// Private dir holding a pasted text's file; removed when the handle
    /// drops (end of `shutdown_share`).
    _cleanup: RemoveDirOnDrop,
}

// ------------------------------------------------------ row model (UI thread)

pub(crate) fn init_model(ui: &AppWindow) {
    ui.global::<State>()
        .set_transfers(ModelRc::from(Rc::new(VecModel::<TransferRow>::default())));
}

fn with_rows<R>(ui: &AppWindow, f: impl FnOnce(&VecModel<TransferRow>) -> R) -> Option<R> {
    let model = ui.global::<State>().get_transfers();
    let rows = model.as_any().downcast_ref::<VecModel<TransferRow>>()?;
    Some(f(rows))
}

fn position(rows: &VecModel<TransferRow>, key: &str) -> Option<usize> {
    (0..rows.row_count()).find(|&i| rows.row_data(i).is_some_and(|r| r.key == key))
}

pub(crate) fn insert_row(ui: &AppWindow, row: TransferRow) {
    with_rows(ui, |rows| {
        // A new transfer replaces finished ones of the same kind for that peer.
        let mut i = 0;
        while i < rows.row_count() {
            match rows.row_data(i) {
                Some(r) if !r.active && r.peer_id == row.peer_id && r.sending == row.sending => {
                    rows.remove(i);
                }
                _ => i += 1,
            }
        }
        rows.push(row);
    });
    sync_peer_activity(ui);
}

pub(crate) fn update_row(ui: &AppWindow, key: &str, f: impl FnOnce(&mut TransferRow)) {
    with_rows(ui, |rows| {
        if let Some(i) = position(rows, key) {
            let mut row = rows.row_data(i).expect("index in range");
            f(&mut row);
            rows.set_row_data(i, row);
        }
    });
    sync_peer_activity(ui);
}

pub(crate) fn remove_row(ui: &AppWindow, key: &str) {
    with_rows(ui, |rows| {
        if let Some(i) = position(rows, key) {
            rows.remove(i);
        }
    });
    sync_peer_activity(ui);
}

/// The peer of a transfer row.
fn row_peer(ui: &AppWindow, key: &str) -> Option<SharedString> {
    with_rows(ui, |rows| position(rows, key).and_then(|i| rows.row_data(i)))
        .flatten()
        .map(|row| row.peer_id)
}

/// `update_row` from any thread.
pub(crate) fn post_update(
    weak: &Weak<AppWindow>,
    key: &str,
    f: impl FnOnce(&mut TransferRow) + Send + 'static,
) {
    let key = key.to_string();
    let _ = weak.upgrade_in_event_loop(move |ui| update_row(&ui, &key, f));
}

/// Mark a row as finished with an error.
pub(crate) fn fail(weak: &Weak<AppWindow>, key: &str, msg: String) {
    post_update(weak, key, move |row| {
        row.active = false;
        row.status = "Failed".into();
        row.speed = "".into();
        row.error = msg.into();
    });
}

/// Mirror in-flight transfers into the peer list (sidebar badges) and the
/// selected peer's "sending" flag.
pub(crate) fn sync_peer_activity(ui: &AppWindow) {
    let state = ui.global::<State>();
    let active: Vec<(SharedString, bool)> = state
        .get_transfers()
        .iter()
        // Queued sends don't block new ones.
        .filter(|t| t.active && !t.waiting)
        .map(|t| (t.peer_id, t.sending))
        .collect();
    let busy = |id: &SharedString, sending: bool| {
        active.iter().any(|(peer, s)| peer == id && *s == sending)
    };
    let peers = state.get_peers();
    for i in 0..peers.row_count() {
        let Some(mut peer) = peers.row_data(i) else {
            continue;
        };
        let (sending, receiving) = (
            busy(&peer.endpoint_id, true),
            busy(&peer.endpoint_id, false),
        );
        if peer.sending != sending || peer.receiving != receiving {
            peer.sending = sending;
            peer.receiving = receiving;
            peers.set_row_data(i, peer);
        }
    }
    let quick: Vec<TransferRow> = state.get_transfers().iter().filter(|t| t.quick).collect();
    set_rows(&state.get_quick_transfers(), quick, |rows| state.set_quick_transfers(rows));

    let selected = state.get_selected_id();
    // Sends to a browser wait on its page; more can join them meanwhile.
    state.set_selected_sending(busy(&selected, true) && !guest::is_guest(&selected));

    // The selected peer's rows. Update in place when the same transfers are
    // shown so cards (and their progress animation) aren't rebuilt.
    let visible: Vec<TransferRow> = state
        .get_transfers()
        .iter()
        .filter(|t| t.peer_id == selected && !t.quick)
        .collect();
    set_rows(&state.get_visible_transfers(), visible, |rows| {
        state.set_visible_transfers(rows)
    });
}

/// Show `rows` in the model `shown`: in place when they are the same
/// transfers, so cards (and their progress animation) aren't rebuilt, else
/// through `replace`.
fn set_rows(
    shown: &ModelRc<TransferRow>,
    rows: Vec<TransferRow>,
    replace: impl FnOnce(ModelRc<TransferRow>),
) {
    let same_rows = shown.row_count() == rows.len()
        && rows
            .iter()
            .enumerate()
            .all(|(i, t)| shown.row_data(i).is_some_and(|s| s.key == t.key));
    if same_rows {
        for (i, row) in rows.into_iter().enumerate() {
            if shown.row_data(i).as_ref() != Some(&row) {
                shown.set_row_data(i, row);
            }
        }
    } else {
        replace(ModelRc::from(Rc::new(VecModel::from(rows))));
    }
}

// ------------------------------------------------------------ engine events

struct TransferEmitter {
    weak: Weak<AppWindow>,
    key: String,
    recorder: Arc<Recorder>,
    /// Sends: closes the share once the peer has everything.
    close_when_sent: Option<AppCtx>,
    /// Receives: what got saved (to recognize pasted text).
    received: Option<Arc<Mutex<Received>>>,
}

impl TransferEmitter {
    fn emit(&self, name: &str, payload: Option<&str>) {
        self.recorder.note(name, payload);
        if let Some(received) = &self.received {
            received.lock().unwrap().note(name, payload);
        }
        let (event, payload_owned) = (name.to_string(), payload.map(str::to_string));
        post_update(&self.weak, &self.key, move |row| {
            apply_transfer_event(row, &event, payload_owned.as_deref());
        });
        if name == "transfer-completed" {
            if let Some(ctx) = &self.close_when_sent {
                let (ctx, key) = (ctx.clone(), self.key.clone());
                ctx.rt
                    .clone()
                    .spawn(async move { finish_send(ctx, key).await });
            }
        }
    }
}

impl EventEmitter for TransferEmitter {
    fn emit_event(&self, event_name: &str) -> Result<(), String> {
        self.emit(event_name, None);
        Ok(())
    }

    fn emit_event_with_payload(&self, event_name: &str, payload: &str) -> Result<(), String> {
        self.emit(event_name, Some(payload));
        Ok(())
    }
}

// --------------------------------------------------------------------- send

pub(crate) fn path_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn dir_size(path: &Path) -> u64 {
    let md = match std::fs::metadata(path) {
        Ok(md) => md,
        Err(_) => return 0,
    };
    if md.is_file() {
        return md.len();
    }
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(md) = entry.metadata() else { continue };
            if md.is_dir() {
                stack.push(entry.path());
            } else {
                total = total.saturating_add(md.len());
            }
        }
    }
    total
}

fn path_type_of(paths: &[PathBuf]) -> Option<crate::engine::TransferPathType> {
    match paths {
        [only] if only.is_dir() => Some(crate::engine::TransferPathType::Directory),
        [_] => Some(crate::engine::TransferPathType::File),
        _ => None,
    }
}

fn metadata_for(paths: &[PathBuf]) -> crate::engine::FileMetadata {
    let mime_type = match paths {
        [only] if only.is_dir() => "inode/directory",
        [_] => "application/octet-stream",
        _ => "application/x-iroh-collection",
    };
    crate::engine::FileMetadata {
        file_name: paths
            .first()
            .map(|p| path_name(p))
            .unwrap_or_else(|| "share".to_string()),
        item_count: paths.len() as u32,
        size: paths.iter().map(|p| dir_size(p)).sum(),
        thumbnail: None,
        mime_type: Some(mime_type.to_string()),
        items: None,
    }
}

/// "report.pdf" for one item, "3 items" for several.
pub(crate) fn items_title(first_name: &str, count: usize) -> String {
    if first_name == PASTE_FILE_NAME && count == 1 {
        "text".to_string()
    } else if count == 1 {
        first_name.to_string()
    } else {
        format!("{count} items")
    }
}

// ------------------------------------------------------------ pasted text

/// Pasted text travels as a single file with this name; a receiver that
/// sees exactly this file shows its text instead of a file row. Other
/// clients just get an ordinary text file.
pub(crate) const PASTE_FILE_NAME: &str = "flipflop-paste.txt";
/// Bigger "pastes" stay plain files.
pub(crate) const PASTE_MAX_BYTES: u64 = 256 * 1024;

/// What a receive saved, from its engine events.
#[derive(Default)]
struct Received {
    names: Vec<String>,
    /// (original, resolved) paths of files renamed to avoid overwriting.
    renamed: Vec<(String, String)>,
}

impl Received {
    fn note(&mut self, event: &str, payload: Option<&str>) {
        let Some(payload) = payload else { return };
        match event {
            "receive-file-names" => {
                self.names = serde_json::from_str(payload).unwrap_or_default();
            }
            "receive-conflicts" => {
                #[derive(serde::Deserialize)]
                struct Conflict {
                    original: String,
                    resolved: String,
                }
                let conflicts: Vec<Conflict> = serde_json::from_str(payload).unwrap_or_default();
                self.renamed = conflicts
                    .into_iter()
                    .map(|c| (c.original, c.resolved))
                    .collect();
            }
            _ => {}
        }
    }

    /// The text of a received paste, if that is what this was.
    fn pasted_text(&self, save_dir: &Path) -> Option<String> {
        if self.names.len() != 1 || self.names[0] != PASTE_FILE_NAME {
            return None;
        }
        let saved = save_dir.join(PASTE_FILE_NAME);
        let saved = self
            .renamed
            .iter()
            .find(|(original, _)| Path::new(original) == saved)
            .map(|(_, resolved)| PathBuf::from(resolved))
            .unwrap_or(saved);
        if std::fs::metadata(&saved).ok()?.len() > PASTE_MAX_BYTES {
            return None;
        }
        std::fs::read_to_string(saved).ok()
    }
}

/// A single http(s) link (worth an "Open" button).
pub(crate) fn as_link(text: &str) -> Option<&str> {
    let text = text.trim();
    let is_link = (text.starts_with("https://") || text.starts_with("http://"))
        && !text.chars().any(char::is_whitespace)
        && text.len() > "https://".len();
    is_link.then_some(text)
}

/// Longest excerpt of a pasted text the history keeps.
const PREVIEW_MAX_CHARS: usize = 120;

/// `text` squeezed onto one line for the history list.
pub(crate) fn text_preview(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match line.char_indices().nth(PREVIEW_MAX_CHARS) {
        Some((cut, _)) => format!("{}…", line[..cut].trim_end()),
        None => line,
    }
}

/// The text of a paste being sent, if `paths` is one.
pub(crate) fn sent_paste_text(paths: &[PathBuf]) -> Option<String> {
    let [path] = paths else { return None };
    if path_name(path) != PASTE_FILE_NAME
        || std::fs::metadata(path).ok()?.len() > PASTE_MAX_BYTES
    {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// Write `text` to a fresh private dir as the paste file; returns (dir, file).
fn write_paste_file(text: &str) -> std::io::Result<(PathBuf, PathBuf)> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let dir = crate::engine::storage::temp_dir().join(format!(".tm-paste-{nanos:x}"));
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(PASTE_FILE_NAME);
    std::fs::write(&file, text)?;
    Ok((dir, file))
}

/// Deletes a directory when dropped, unless taken.
struct RemoveDirOnDrop(Option<PathBuf>);

impl Drop for RemoveDirOnDrop {
    fn drop(&mut self) {
        if let Some(dir) = self.0.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Take what the composer holds, the typed text and then the attached
/// files, as sends; the text box is cleared and desktop picks are taken.
/// Empty when there is nothing. UI thread.
pub(crate) fn take_composed(ctx: &AppCtx, ui: &AppWindow) -> Vec<Outgoing> {
    let state = ui.global::<State>();
    let text = state.get_paste_input().to_string();
    let mut sends = Vec::new();
    if !text.trim().is_empty() {
        match write_paste_file(&text) {
            Ok((dir, file)) => {
                state.set_paste_input("".into());
                sends.push(Outgoing {
                    paths: vec![file],
                    staged: Vec::new(),
                    cleanup: Some(dir),
                });
            }
            Err(e) => toast(ui, &format!("Could not prepare the text: {e}"), true),
        }
    }
    let picked = std::mem::take(&mut *ctx.transfers.picked.lock().unwrap());
    let staged = queued_staged(ctx);
    let paths: Vec<PathBuf> = staged.iter().chain(&picked).cloned().collect();
    if !paths.is_empty() {
        sends.push(Outgoing {
            paths,
            staged,
            cleanup: None,
        });
    }
    if !picked.is_empty() {
        refresh_outbox(ctx);
    }
    sends
}

/// Share `paths` and deliver the ticket to the peer. UI thread.
fn start_send(
    ctx: &AppCtx,
    ui: &AppWindow,
    node: Arc<NodeService>,
    peer_id: String,
    peer_name: String,
    paths: Vec<PathBuf>,
    staged: Vec<PathBuf>,
    cleanup: Option<PathBuf>,
) {
    if guest::is_guest(&peer_id) {
        return guest::offer_paths(ctx, ui, &peer_id, paths, staged, cleanup);
    }
    let key = ctx.transfers.new_key("send");
    let first_name = paths.first().map(|p| path_name(p)).unwrap_or_default();
    insert_row(
        ui,
        TransferRow {
            key: key.clone().into(),
            peer_id: peer_id.clone().into(),
            sending: true,
            active: true,
            title: items_title(&first_name, paths.len()).into(),
            status: "Preparing…".into(),
            ..Default::default()
        },
    );
    claim(ctx, &staged);
    spawn_send(ctx, node, key, peer_id, peer_name, paths, staged, cleanup);
}

/// A share [`open_share`] opened.
struct Opened {
    ticket: String,
    item_count: u32,
    byte_count: u64,
    /// The text, when this is a pasted one.
    pasted: Option<String>,
}

/// Import `paths` and open a share of them for `peer_id`, registered under
/// the row `key` so Stop and completion find it. `staged` share-sheet files
/// must be claimed; the share owns them (and `cleanup`) from here. `None`
/// when that failed (the row says why) or Stop came first.
#[allow(clippy::too_many_arguments)]
async fn open_share(
    ctx: &AppCtx,
    node: &NodeService,
    key: &str,
    peer_id: &str,
    peer_name: &str,
    paths: Vec<PathBuf>,
    staged: Vec<PathBuf>,
    cleanup: Option<PathBuf>,
) -> Option<Opened> {
    // Removed on any early return; the open share owns it after that.
    let mut cleanup = RemoveDirOnDrop(cleanup);
    let mut claim_guard = ClaimGuard {
        ctx: ctx.clone(),
        paths: staged.clone(),
    };
    let weak = &ctx.weak;
    // Walking folders for their size can take a while: keep it off the
    // async workers.
    let scan = paths.clone();
    let (metadata, path_type, pasted) = match tokio::task::spawn_blocking(move || {
        (metadata_for(&scan), path_type_of(&scan), sent_paste_text(&scan))
    })
    .await
    {
        Ok(v) => v,
        Err(e) => {
            fail(weak, key, format!("Could not read the files: {e}"));
            return None;
        }
    };
    let (byte_count, item_count) = (metadata.size, metadata.item_count);
    post_update(weak, key, move |row| {
        row.progress_label = fmt_bytes(byte_count).into();
    });

    let history_enabled = ctx.settings.lock().unwrap().history_enabled;
    let recorder = Arc::new(Recorder::new(
        ctx.history.clone(),
        TransferDirection::Send,
        Ctx {
            root_name: metadata.file_name.clone(),
            payload_bytes: byte_count,
            item_count,
            path_type,
            peer: Some(TransferPeer {
                endpoint_id: peer_id.to_string(),
                display_name: Some(peer_name.to_string()),
                device_type: None,
            }),
            text: pasted.clone(),
            ..Ctx::default()
        },
        history_enabled,
    ));
    let emitter = TransferEmitter {
        weak: weak.clone(),
        key: key.to_string(),
        recorder: recorder.clone(),
        close_when_sent: Some(ctx.clone()),
        received: None,
    };
    let app_handle: AppHandle = Some(Arc::new(emitter));

    let share = match node.share_with_peer(peer_id, paths, app_handle).await {
        Ok(share) => share,
        Err(e) => {
            tracing::warn!(%peer_id, "send: share setup failed: {e:#}");
            forget_stopped(ctx, key).await;
            fail(weak, key, format!("Could not start share: {e:#}"));
            return None;
        }
    };
    let ticket = share.ticket.clone();
    let handle = ShareHandle {
        _share: share,
        recorder,
        peer_name: peer_name.to_string(),
        staged,
        _cleanup: RemoveDirOnDrop(cleanup.0.take()),
    };
    let mut shares = ctx.transfers.shares.lock().await;
    if shares.stopped.remove(key) {
        // Stopped while preparing: the row is already gone.
        drop(shares);
        shutdown_share(ctx, handle, false).await;
        return None;
    }
    shares.open.insert(key.to_string(), handle);
    // The open share now owns the files (see `shutdown_share`).
    claim_guard.disarm();
    Some(Opened {
        ticket,
        item_count,
        byte_count,
        pasted,
    })
}

/// The share-and-invite part of a send whose row exists and whose `staged`
/// files are claimed. An unreachable peer leaves the send waiting (see
/// [`retry_waiting`]).
fn spawn_send(
    ctx: &AppCtx,
    node: Arc<NodeService>,
    key: String,
    peer_id: String,
    peer_name: String,
    paths: Vec<PathBuf>,
    staged: Vec<PathBuf>,
    cleanup: Option<PathBuf>,
) {
    tracing::info!(%peer_id, %key, paths = paths.len(), "send: starting flow");
    let ctx = ctx.clone();
    let weak = ctx.weak.clone();
    ctx.rt.clone().spawn(async move {
        let saved_paths = paths.clone();
        let saved_staged = staged.clone();
        let Some(Opened {
            ticket,
            item_count,
            byte_count,
            pasted,
        }) = open_share(&ctx, &node, &key, &peer_id, &peer_name, paths, staged, cleanup).await
        else {
            unqueue(&ctx, &key);
            return;
        };
        // Kept for the saved queue in case the peer can't be reached.
        let queued = QueuedSend {
            peer_id: peer_id.clone(),
            peer_name: peer_name.clone(),
            paths: if pasted.is_some() {
                Vec::new()
            } else {
                saved_paths
            },
            staged: saved_staged,
            text: pasted,
        };

        // A peer known to be offline is not worth a connection timeout now;
        // the send waits for it either way.
        let online = node
            .list_paired()
            .ok()
            .and_then(|devices| {
                devices
                    .into_iter()
                    .find(|d| d.endpoint_id.eq_ignore_ascii_case(&peer_id))
            })
            .is_some_and(|d| d.online);
        let delivered = if online {
            post_update(&weak, &key, |row| row.status = "Contacting peer…".into());
            match node
                .invite_paired_device(&peer_id, &ticket, item_count, byte_count)
                .await
            {
                Ok(delivered) => delivered,
                Err(e) => {
                    tracing::warn!(%peer_id, "send: invite delivery failed: {e:#}");
                    false
                }
            }
        } else {
            false
        };
        if delivered {
            tracing::info!(%peer_id, "send: invite delivered, waiting for peer to pull");
            unqueue(&ctx, &key);
            post_update(&weak, &key, move |row| {
                row.waiting = false;
                // Progress events may already have moved the status on.
                if row.status.as_str() == "Contacting peer…" {
                    row.status = format!("Delivered, waiting for {peer_name} to download…").into();
                }
            });
            return;
        }
        tracing::info!(%peer_id, "send: peer unreachable, waiting for it");
        {
            // Unless Stop came first: then the share is already gone.
            let shares = ctx.transfers.shares.lock().await;
            if !shares.open.contains_key(&key) {
                return;
            }
            // Posted before a retry can see the entry, so a retry's
            // "Delivered" update always lands after it.
            let name = peer_name.clone();
            post_update(&weak, &key, move |row| {
                row.waiting = true;
                row.status = format!("Waiting for {name} to come online").into();
            });
            ctx.transfers.waiting.lock().unwrap().insert(
                key.clone(),
                PendingInvite {
                    peer_id,
                    peer_name: peer_name.clone(),
                    ticket,
                    item_count,
                    byte_count,
                    attempting: false,
                },
            );
            ctx.transfers
                .queued
                .lock()
                .unwrap()
                .insert(key.clone(), queued);
        }
        save_queue(&ctx);
    });
}

// ------------------------------------------------------------- send queue

/// How often waiting sends retry regardless of presence events.
const QUEUE_RETRY_INTERVAL: Duration = Duration::from_secs(60);

/// Try again to deliver the invites of waiting sends: all of them, or only
/// `peer`'s (it just came online). Callable from any thread.
pub(crate) fn retry_waiting(ctx: &AppCtx, peer: Option<&str>) {
    let Some(node) = ctx.node() else { return };
    let due: Vec<(String, String, String, String, u32, u64)> = {
        let mut waiting = ctx.transfers.waiting.lock().unwrap();
        waiting
            .iter_mut()
            .filter(|(_, w)| {
                !w.attempting && peer.is_none_or(|p| w.peer_id.eq_ignore_ascii_case(p))
            })
            .map(|(key, w)| {
                w.attempting = true;
                (
                    key.clone(),
                    w.peer_id.clone(),
                    w.peer_name.clone(),
                    w.ticket.clone(),
                    w.item_count,
                    w.byte_count,
                )
            })
            .collect()
    };
    for (key, peer_id, peer_name, ticket, item_count, byte_count) in due {
        let ctx = ctx.clone();
        let node = node.clone();
        ctx.rt.clone().spawn(async move {
            let delivered = node
                .invite_paired_device(&peer_id, &ticket, item_count, byte_count)
                .await
                .unwrap_or(false);
            if !delivered {
                if let Some(w) = ctx.transfers.waiting.lock().unwrap().get_mut(&key) {
                    w.attempting = false;
                }
                return;
            }
            // Stopped meanwhile: nothing is waiting any more.
            if ctx.transfers.waiting.lock().unwrap().remove(&key).is_none() {
                return;
            }
            tracing::info!(%peer_id, %key, "send: queued invite delivered");
            unqueue(&ctx, &key);
            post_update(&ctx.weak, &key, move |row| {
                row.waiting = false;
                row.status = format!("Delivered, waiting for {peer_name} to download…").into();
            });
        });
    }
}

/// Drop a send from the saved queue. Returns it if it was there.
fn unqueue(ctx: &AppCtx, key: &str) -> Option<QueuedSend> {
    let removed = ctx.transfers.queued.lock().unwrap().remove(key);
    if removed.is_some() {
        save_queue(ctx);
    }
    removed
}

fn save_queue(ctx: &AppCtx) {
    let Some(path) = ctx.transfers.queue_path.get() else {
        return;
    };
    let sends: Vec<QueuedSend> = ctx
        .transfers
        .queued
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect();
    let result = if sends.is_empty() {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    } else {
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&sends).expect("queue serializes"),
        )
    };
    if let Err(e) = result {
        tracing::warn!("saving the send queue failed: {e}");
    }
}

/// Bring back the sends that were waiting when the app last closed (saved
/// at `path`): their rows show at once, and they restart once the node is
/// up ([`start_restored`]). UI thread, before the share-sheet queue is shown.
pub(crate) fn restore_queued(ctx: &AppCtx, ui: &AppWindow, path: PathBuf) {
    let saved: Vec<QueuedSend> = std::fs::read(&path)
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or_default();
    let _ = ctx.transfers.queue_path.set(path);
    let mut dropped = 0;
    for send in saved {
        let files_gone = send.paths.is_empty() || !send.paths.iter().all(|p| p.exists());
        if send.text.is_none() && files_gone {
            dropped += 1;
            continue;
        }
        let key = ctx.transfers.new_key("send");
        let title = match &send.text {
            Some(_) => items_title(PASTE_FILE_NAME, 1),
            None => items_title(&path_name(&send.paths[0]), send.paths.len()),
        };
        insert_row(
            ui,
            TransferRow {
                key: key.clone().into(),
                peer_id: send.peer_id.clone().into(),
                sending: true,
                active: true,
                waiting: true,
                title: title.into(),
                status: format!("Waiting for {} to come online", send.peer_name).into(),
                ..Default::default()
            },
        );
        claim(ctx, &send.staged);
        ctx.transfers.restored.lock().unwrap().insert(key.clone());
        ctx.transfers.queued.lock().unwrap().insert(key, send);
    }
    if dropped > 0 {
        save_queue(ctx);
        let what = if dropped == 1 {
            "A queued send was"
        } else {
            "Some queued sends were"
        };
        toast(ui, &format!("{what} dropped: its files are gone"), true);
    }
}

/// Set up the shares of the sends [`restore_queued`] brought back. Call
/// once the node is running; UI thread.
pub(crate) fn start_restored(ctx: &AppCtx) {
    let Some(node) = ctx.node() else { return };
    let keys: Vec<String> = ctx.transfers.restored.lock().unwrap().drain().collect();
    for key in keys {
        let Some(send) = ctx.transfers.queued.lock().unwrap().get(&key).cloned() else {
            continue;
        };
        let (paths, cleanup) = match &send.text {
            Some(text) => match write_paste_file(text) {
                Ok((dir, file)) => (vec![file], Some(dir)),
                Err(e) => {
                    release(ctx, &send.staged);
                    unqueue(ctx, &key);
                    fail(&ctx.weak, &key, format!("Could not prepare the text: {e}"));
                    continue;
                }
            },
            None => (send.paths.clone(), None),
        };
        spawn_send(
            ctx,
            node.clone(),
            key,
            send.peer_id,
            send.peer_name,
            paths,
            send.staged,
            cleanup,
        );
    }
}

async fn take_share(ctx: &AppCtx, key: &str) -> Option<ShareHandle> {
    ctx.transfers.shares.lock().await.open.remove(key)
}

async fn forget_stopped(ctx: &AppCtx, key: &str) {
    ctx.transfers.shares.lock().await.stopped.remove(key);
}

/// Stop serving the share. Its share-sheet files are deleted once the peer
/// has them (`sent`); otherwise they go back to the queue for a retry.
async fn shutdown_share(ctx: &AppCtx, handle: ShareHandle, sent: bool) {
    let staged = handle.staged.clone();
    drop(handle);
    if staged.is_empty() {
        return;
    }
    if sent {
        android::clear_outbox(&staged);
    }
    release(ctx, &staged);
}

// ---------------------------------------------------- share-sheet queue

/// Queued share-sheet files: staged, and not taken by a send in flight.
fn queued_staged(ctx: &AppCtx) -> Vec<PathBuf> {
    let claimed = ctx.transfers.claimed.lock().unwrap();
    android::outbox_files()
        .into_iter()
        .filter(|p| !claimed.contains(p))
        .collect()
}

/// Everything attached in the composer: queued share-sheet files, then
/// desktop picks.
fn queued_files(ctx: &AppCtx) -> Vec<PathBuf> {
    let mut files = queued_staged(ctx);
    files.extend(ctx.transfers.picked.lock().unwrap().iter().cloned());
    files
}

pub(crate) fn claim(ctx: &AppCtx, paths: &[PathBuf]) {
    ctx.transfers
        .claimed
        .lock()
        .unwrap()
        .extend(paths.iter().cloned());
    refresh_outbox(ctx);
}

pub(crate) fn release(ctx: &AppCtx, paths: &[PathBuf]) {
    {
        let mut claimed = ctx.transfers.claimed.lock().unwrap();
        for path in paths {
            claimed.remove(path);
        }
    }
    refresh_outbox(ctx);
}

/// Returns a send's files to the queue unless the send got far enough to
/// own them (`disarm`), e.g. when share setup fails.
struct ClaimGuard {
    ctx: AppCtx,
    paths: Vec<PathBuf>,
}

impl ClaimGuard {
    fn disarm(&mut self) {
        self.paths.clear();
    }
}

impl Drop for ClaimGuard {
    fn drop(&mut self) {
        if !self.paths.is_empty() {
            release(&self.ctx, &self.paths);
        }
    }
}

/// Mirror the queue into `State.outbox`. Callable from any thread.
pub(crate) fn refresh_outbox(ctx: &AppCtx) {
    let rows: Vec<(String, String, String)> = queued_files(ctx)
        .iter()
        .map(|p| {
            let size = dir_size(p);
            (
                p.to_string_lossy().into_owned(),
                path_name(p),
                fmt_bytes(size),
            )
        })
        .collect();
    let _ = ctx.weak.upgrade_in_event_loop(move |ui| {
        let state = ui.global::<State>();
        state.set_outbox_count(rows.len() as i32);
        let rows: Vec<OutboxRow> = rows
            .into_iter()
            .map(|(path, name, size)| OutboxRow {
                path: path.into(),
                name: name.into(),
                size: size.into(),
            })
            .collect();
        state.set_outbox(ModelRc::from(Rc::new(VecModel::from(rows))));
    });
}

/// The peer has the files: close the share and settle the row.
async fn finish_send(ctx: AppCtx, key: String) {
    // Let the last acknowledgements flush before closing the share.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let Some(handle) = take_share(&ctx, &key).await else {
        return;
    };
    let peer_name = handle.peer_name.clone();
    shutdown_share(&ctx, handle, true).await;
    let ctx_ui = ctx.clone();
    let _ = ctx.weak.upgrade_in_event_loop(move |ui| {
        // Done: the history row is its record from here on.
        remove_row(&ui, &key);
        toast(&ui, &format!("Sent to {peer_name}"), false);
        refresh_history(&ctx_ui);
    });
}

/// User pressed Stop on a send.
fn stop_send(ctx: &AppCtx, key: String) {
    let peer = ctx.weak.upgrade().and_then(|ui| row_peer(&ui, &key));
    if let Some(peer) = peer.filter(|peer| guest::is_guest(peer)) {
        return guest::withdraw(ctx, &peer, &key);
    }
    // Restored and not started yet: there is no share to stop.
    if ctx.transfers.restored.lock().unwrap().remove(&key) {
        if let Some(send) = unqueue(ctx, &key) {
            release(ctx, &send.staged);
        }
        if let Some(ui) = ctx.weak.upgrade() {
            remove_row(&ui, &key);
            toast(&ui, "Send cancelled", false);
        }
        return;
    }
    let ctx = ctx.clone();
    ctx.rt.clone().spawn(async move {
        let handle = {
            let mut shares = ctx.transfers.shares.lock().await;
            let handle = shares.open.remove(&key);
            if handle.is_none() {
                // Still preparing (or already gone): tell the setup task.
                shares.stopped.insert(key.clone());
            }
            handle
        };
        // After the share is gone, so a send can't start waiting again.
        ctx.transfers.waiting.lock().unwrap().remove(&key);
        unqueue(&ctx, &key);
        if let Some(handle) = handle {
            // Finalize first so the abort's own "transfer-failed" can't win.
            handle
                .recorder
                .finalize(TransferStatus::Cancelled, None, None, None, None);
            shutdown_share(&ctx, handle, false).await;
        }
        let ctx_ui = ctx.clone();
        let _ = ctx.weak.upgrade_in_event_loop(move |ui| {
            remove_row(&ui, &key);
            toast(&ui, "Send stopped", false);
            refresh_history(&ctx_ui);
        });
    });
}

// ------------------------------------------------------------------ receive

#[derive(Clone)]
struct Invite {
    ticket: String,
    file_count: u32,
    total_size: u64,
    sender_name: String,
    peer_id: String,
}

fn parse_invite_payload(v: &serde_json::Value) -> Invite {
    let str_of = |key: &str| {
        v.get(key)
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string()
    };
    Invite {
        ticket: str_of("blob_ticket"),
        file_count: v.get("file_count").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
        total_size: v.get("total_size").and_then(|x| x.as_u64()).unwrap_or(0),
        sender_name: str_of("sender_name"),
        peer_id: str_of("remote_endpoint_id").to_lowercase(),
    }
}

/// Display name for an inviting peer: our stored name, else the name the
/// invite claims, else a short id. Names equal to the raw id don't count.
fn invite_peer_name(stored: Option<String>, claimed: &str, peer_id: &str) -> String {
    let usable = |n: &str| !n.trim().is_empty() && n.trim().to_lowercase() != peer_id;
    stored
        .filter(|n| usable(n))
        .or_else(|| usable(claimed).then(|| claimed.trim().to_string()))
        .unwrap_or_else(|| short_id(peer_id))
}

/// Where a resumable download of `ticket` keeps its partial store.
fn partial_store_path(ticket: &str) -> Option<String> {
    let ticket = iroh_blobs::ticket::BlobTicket::from_str(ticket).ok()?;
    let dir = format!(
        "{}{}",
        crate::engine::storage::RECV_DIR_PREFIX,
        ticket.hash().to_hex()
    );
    Some(
        crate::engine::storage::temp_dir()
            .join(dir)
            .to_string_lossy()
            .into_owned(),
    )
}

/// Where files from `peer_name` are saved: a folder named after it in the
/// downloads folder.
pub(crate) fn save_dir(ctx: &AppCtx, peer_name: &str, peer_id: &str) -> PathBuf {
    let save_dir = ctx
        .settings
        .lock()
        .unwrap()
        .downloads_path()
        .unwrap_or_else(|| platform::default_data_dir().join("downloads"))
        .join(sanitize_folder_name(peer_name, &short_id(peer_id)));
    // The blob store only exports to absolute paths (a relative one fails as
    // a bare "Error::Io"), and a folder typed into Settings may be relative.
    std::path::absolute(&save_dir).unwrap_or(save_dir)
}

/// A paired peer sent us files: accept and download them, no prompt.
/// UI thread (called from node event routing).
pub(crate) fn accept_invite(ctx: &AppCtx, payload: serde_json::Value) {
    let invite = parse_invite_payload(&payload);
    if invite.peer_id.is_empty() || invite.ticket.is_empty() {
        return;
    }
    let items = if invite.file_count == 1 {
        "1 item".to_string()
    } else {
        format!("{} items", invite.file_count)
    };
    receive(ctx, invite, items, false);
}

/// Download what `invite` offers, on a new row titled `title`. A `quick`
/// share's sender is no peer of ours: it isn't answered (the download is
/// the answer), and its finished row stays on the Quick share page.
/// UI thread.
fn receive(ctx: &AppCtx, invite: Invite, title: String, quick: bool) {
    let Some(node) = ctx.node() else {
        return;
    };
    let Some(ui) = ctx.weak.upgrade() else {
        return;
    };

    let key = ctx.transfers.new_key("recv");
    insert_row(
        &ui,
        TransferRow {
            key: key.clone().into(),
            peer_id: invite.peer_id.clone().into(),
            sending: false,
            active: true,
            quick,
            title: title.into(),
            progress_label: fmt_bytes(invite.total_size).into(),
            status: "Accepting…".into(),
            ..Default::default()
        },
    );
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
    ctx.transfers
        .recv_cancels
        .lock()
        .unwrap()
        .insert(key.clone(), cancel_tx);

    let ctx = ctx.clone();
    ctx.rt.clone().spawn(async move {
        let weak = ctx.weak.clone();
        let id = invite.peer_id.clone();
        tracing::info!(%id, %key, quick, "receive: accepting invite");
        let stored_name = node
            .list_paired()
            .ok()
            .and_then(|list: Vec<PairedDeviceInfo>| {
                list.into_iter()
                    .find(|d| d.endpoint_id.to_lowercase() == id)
                    .map(|d| d.display_name)
            })
            .filter(|_| !quick);
        let peer_name = invite_peer_name(stored_name, &invite.sender_name, &id);
        let save_dir = save_dir(&ctx, &peer_name, &id);
        let history_enabled = ctx.settings.lock().unwrap().history_enabled;
        if let Err(e) = std::fs::create_dir_all(&save_dir) {
            tracing::warn!(save = %save_dir.display(), "receive: cannot create save dir: {e}");
        }

        let give_up = |msg: String| {
            ctx.transfers.recv_cancels.lock().unwrap().remove(&key);
            toast_later(&weak, msg.clone(), true);
            fail(&weak, &key, msg);
        };

        if !quick {
            if let Err(e) = node.respond_paired_invite(&id, true).await {
                tracing::warn!(%id, "receive: accept handshake failed: {e}");
                return give_up(format!("Could not accept files from {peer_name}: {e}"));
            }
        }
        let recorder = Arc::new(Recorder::new(
            ctx.history.clone(),
            TransferDirection::Receive,
            Ctx {
                payload_bytes: invite.total_size,
                item_count: invite.file_count,
                save_path: Some(save_dir.to_string_lossy().into_owned()),
                peer: Some(TransferPeer {
                    endpoint_id: id.clone(),
                    display_name: Some(peer_name.clone()),
                    device_type: None,
                }),
                blob_hash: iroh_blobs::ticket::BlobTicket::from_str(&invite.ticket)
                    .ok()
                    .map(|t| t.hash().to_hex().to_string()),
                resumable_store_path: partial_store_path(&invite.ticket),
                ..Ctx::default()
            },
            history_enabled,
        ));
        let received = Arc::new(Mutex::new(Received::default()));
        let app_handle: AppHandle = Some(Arc::new(TransferEmitter {
            weak: weak.clone(),
            key: key.clone(),
            recorder: recorder.clone(),
            close_when_sent: None,
            received: Some(received.clone()),
        }));
        toast_later(&weak, format!("{peer_name} is sending you files"), false);
        post_update(&weak, &key, |row| row.status = "Connecting…".into());

        let result = node
            .download_from_peer(&id, &invite.ticket, save_dir.clone(), app_handle, cancel_rx)
            .await;
        let cancelled = ctx
            .transfers
            .recv_cancels
            .lock()
            .unwrap()
            .remove(&key)
            .is_none();
        let ctx_ui = ctx.clone();
        match result {
            Ok(_) => {
                // Pasted text goes into the history row (the file stays
                // saved too).
                let pasted = received.lock().unwrap().pasted_text(&save_dir);
                if let Some(text) = &pasted {
                    recorder.set_text(text.clone());
                }
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    if quick {
                        // No peer page to keep the record: the row stays.
                        finish_quick_receive(&ui, &key, &peer_name, &save_dir, pasted);
                        return;
                    }
                    // Done: the history row is its record from here on.
                    remove_row(&ui, &key);
                    let msg = if pasted.is_some() {
                        format!("Text from {peer_name}")
                    } else {
                        format!("Files from {peer_name} saved")
                    };
                    toast(&ui, &msg, false);
                    refresh_history(&ctx_ui);
                });
            }
            Err(_) if cancelled => {
                recorder.finalize(TransferStatus::Cancelled, None, None, None, None);
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    remove_row(&ui, &key);
                    toast(&ui, "Receive cancelled", false);
                    refresh_history(&ctx_ui);
                });
            }
            Err(e) => {
                recorder.finalize(
                    TransferStatus::Failed,
                    None,
                    None,
                    None,
                    Some(format!("{e:#}")),
                );
                tracing::warn!(%id, "receive: failed: {e:#}");
                let msg = format!("Receive from {peer_name} failed: {e:#}");
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    toast(&ui, &msg, true);
                    update_row(&ui, &key, |row| {
                        row.active = false;
                        row.status = "Failed".into();
                        row.speed = "".into();
                        row.error = msg.into();
                    });
                    refresh_history(&ctx_ui);
                });
            }
        }
    });
}

// -------------------------------------------------------------- quick share

/// Settle a finished quick receive: its row stays, saying where the files
/// went; a text is copied, there being nowhere else to read it.
fn finish_quick_receive(
    ui: &AppWindow,
    key: &str,
    peer_name: &str,
    save_dir: &Path,
    pasted: Option<String>,
) {
    let (status, msg) = match pasted {
        Some(text) => match platform::copy_to_clipboard(&text) {
            Ok(()) => (
                format!("Text copied · {}", text_preview(&text)),
                format!("Text from {peer_name} copied"),
            ),
            Err(_) => (
                format!("Text saved in {}", save_dir.display()),
                format!("Text from {peer_name} saved"),
            ),
        },
        None => (
            format!("Saved in {}", save_dir.display()),
            format!("Files from {peer_name} saved"),
        ),
    };
    update_row(ui, key, |row| {
        row.active = false;
        row.progress = 1.0;
        row.speed = "".into();
        row.status = status.into();
    });
    toast(ui, &msg, false);
}

/// Mirror the quick shares offered to us into `State.quick-incoming`.
/// UI thread.
fn show_incoming(ctx: &AppCtx, ui: &AppWindow) {
    let rows: Vec<QuickOffer> = ctx
        .transfers
        .incoming
        .lock()
        .unwrap()
        .iter()
        .map(|offer| {
            let name = invite_peer_name(None, &offer.invite.sender_name, &offer.invite.peer_id);
            QuickOffer {
                key: offer.key.clone().into(),
                initials: crate::app::initials(&name).into(),
                sender: name.into(),
                title: offer.title.clone().into(),
                size: fmt_bytes(offer.invite.total_size).into(),
            }
        })
        .collect();
    ui.global::<State>()
        .set_quick_incoming(ModelRc::from(Rc::new(VecModel::from(rows))));
}

/// Someone offered us a quick share (`quick-share-received`): list it for
/// Accept or Decline. UI thread.
pub(crate) fn quick_offered(ctx: &AppCtx, payload: serde_json::Value) {
    let invite = parse_invite_payload(&payload);
    let id = payload.get("id").and_then(|v| v.as_str()).unwrap_or_default();
    if invite.peer_id.is_empty() || invite.ticket.is_empty() || id.is_empty() {
        return;
    }
    let title = payload
        .get("title")
        .and_then(|v| v.as_str())
        .filter(|t| !t.trim().is_empty())
        .map(|t| t.chars().take(200).collect())
        .unwrap_or_else(|| items_title("", invite.file_count as usize));
    let name = invite_peer_name(None, &invite.sender_name, &invite.peer_id);
    ctx.transfers.incoming.lock().unwrap().push(IncomingQuick {
        key: ctx.transfers.new_key("offer"),
        id: id.chars().take(64).collect(),
        title: title.clone(),
        invite,
    });
    let Some(ui) = ctx.weak.upgrade() else { return };
    show_incoming(ctx, &ui);
    let page = if platform::TOUCH { "Share" } else { "Quick share" };
    toast(&ui, &format!("{name} wants to send you {title}. See {page}"), false);
}

/// Accept (download) or decline the quick share `key`. UI thread.
fn answer_quick(ctx: &AppCtx, key: &str, accept: bool) {
    let offer = {
        let mut incoming = ctx.transfers.incoming.lock().unwrap();
        let Some(i) = incoming.iter().position(|o| o.key == key) else {
            return;
        };
        incoming.remove(i)
    };
    if let Some(ui) = ctx.weak.upgrade() {
        show_incoming(ctx, &ui);
    }
    if accept {
        return receive(ctx, offer.invite, offer.title, true);
    }
    let Some(node) = ctx.node() else { return };
    ctx.rt.spawn(async move {
        if let Err(e) = node.decline_quick_share(&offer.invite.peer_id, &offer.id).await {
            tracing::debug!("declining quick share failed: {e:#}");
        }
    });
}

/// Quick share what the composer holds with a device we don't pair with:
/// share it, then offer it to them to accept. UI thread.
fn quick_send(ctx: &AppCtx, peer_id: &str) {
    let Some(ui) = ctx.weak.upgrade() else { return };
    let state = ui.global::<State>();
    let peer_name = state
        .get_nearby_devices()
        .iter()
        .find(|d| d.endpoint_id == peer_id)
        .map(|d| d.name.to_string())
        .unwrap_or_else(|| short_id(peer_id));
    let Some(node) = ctx.node() else {
        toast(&ui, "Still connecting. Try again in a moment.", false);
        return;
    };
    let sends = take_composed(ctx, &ui);
    if sends.is_empty() {
        toast(&ui, "Add something to send first", false);
        return;
    }
    for send in sends {
        let key = ctx.transfers.new_key("send");
        let first_name = send.paths.first().map(|p| path_name(p)).unwrap_or_default();
        let title = items_title(&first_name, send.paths.len());
        insert_row(
            &ui,
            TransferRow {
                key: key.clone().into(),
                peer_id: peer_id.into(),
                sending: true,
                active: true,
                quick: true,
                title: format!("{title} to {peer_name}").into(),
                status: "Preparing…".into(),
                ..Default::default()
            },
        );
        claim(ctx, &send.staged);
        let (ctx, node, peer_id, peer_name) =
            (ctx.clone(), node.clone(), peer_id.to_string(), peer_name.clone());
        ctx.rt.clone().spawn(async move {
            let weak = ctx.weak.clone();
            let Some(opened) = open_share(
                &ctx,
                &node,
                &key,
                &peer_id,
                &peer_name,
                send.paths,
                send.staged,
                send.cleanup,
            )
            .await
            else {
                return;
            };
            post_update(&weak, &key, |row| row.status = "Contacting…".into());
            let delivered = node
                .quick_share(
                    &peer_id,
                    &key,
                    &opened.ticket,
                    opened.item_count,
                    opened.byte_count,
                    &title,
                )
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(%peer_id, "quick share: delivery failed: {e:#}");
                    false
                });
            if delivered {
                post_update(&weak, &key, move |row| {
                    if row.status.as_str() == "Contacting…" {
                        row.status = format!("Waiting for {peer_name} to accept…").into();
                    }
                });
                return;
            }
            if let Some(handle) = take_share(&ctx, &key).await {
                handle.recorder.finalize(
                    TransferStatus::Failed,
                    None,
                    None,
                    None,
                    Some("unreachable".to_string()),
                );
                shutdown_share(&ctx, handle, false).await;
            }
            fail(
                &weak,
                &key,
                format!("Couldn't reach {peer_name}. Are you on the same network?"),
            );
        });
    }
}

/// The receiver turned down our quick share `id` (`quick-share-declined`):
/// stop sharing it. UI thread.
pub(crate) fn quick_declined(ctx: &AppCtx, payload: serde_json::Value) {
    let field = |k: &str| {
        payload
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let (peer_id, key) = (field("remote_endpoint_id").to_lowercase(), field("id"));
    let Some(ui) = ctx.weak.upgrade() else { return };
    // Only a quick send of ours, to that very device.
    let ours = with_rows(&ui, |rows| {
        position(rows, &key)
            .and_then(|i| rows.row_data(i))
            .is_some_and(|r| r.quick && r.sending && r.peer_id.to_lowercase() == peer_id)
    });
    if ours != Some(true) {
        return;
    }
    let ctx = ctx.clone();
    ctx.rt.clone().spawn(async move {
        let Some(handle) = take_share(&ctx, &key).await else {
            return;
        };
        let name = handle.peer_name.clone();
        handle
            .recorder
            .finalize(TransferStatus::Cancelled, None, None, None, None);
        shutdown_share(&ctx, handle, false).await;
        let ctx_ui = ctx.clone();
        let _ = ctx.weak.upgrade_in_event_loop(move |ui| {
            remove_row(&ui, &key);
            toast(&ui, &format!("{name} declined"), true);
            refresh_history(&ctx_ui);
        });
    });
}

// ---------------------------------------------------------------- callbacks

/// Runs the blocking file dialog off the UI thread, then sends what was
/// picked to the selected peer.
#[cfg(not(target_os = "android"))]
fn pick_and_send(ctx: &AppCtx) {
    let Some(ui) = ctx.weak.upgrade() else { return };
    let Some(node) = ctx.node() else {
        toast(&ui, "Still connecting. Try again in a moment.", false);
        return;
    };
    let state = ui.global::<State>();
    let peer_id = state.get_selected_id().to_string();
    let peer_name = state.get_selected_name().to_string();
    if peer_id.is_empty() || state.get_selected_sending() {
        return;
    }
    if ctx.transfers.picking.swap(true, Ordering::SeqCst) {
        return;
    }
    let ctx = ctx.clone();
    ctx.rt.clone().spawn_blocking(move || {
        let picked = platform::pick_send_paths();
        ctx.transfers.picking.store(false, Ordering::SeqCst);
        if picked.is_empty() {
            return;
        }
        let ctx_ui = ctx.clone();
        let _ = ctx.weak.upgrade_in_event_loop(move |ui| {
            start_send(
                &ctx_ui,
                &ui,
                node,
                peer_id,
                peer_name,
                picked,
                Vec::new(),
                None,
            );
        });
    });
}

pub(crate) fn register(ctx: &AppCtx) {
    let Some(ui) = ctx.weak.upgrade() else {
        return;
    };
    let logic = ui.global::<Logic>();

    {
        // Presence can miss a peer coming back (or call it online while it
        // still isn't reachable): keep knocking for waiting sends.
        let ctx = ctx.clone();
        ctx.rt.clone().spawn(async move {
            let mut ticks = tokio::time::interval(QUEUE_RETRY_INTERVAL);
            ticks.tick().await;
            loop {
                ticks.tick().await;
                retry_waiting(&ctx, None);
            }
        });
    }

    {
        // Send card: the typed/pasted text and any queued files, to the
        // selected peer.
        let ctx = ctx.clone();
        logic.on_send_composed(move || {
            let Some(ui) = ctx.weak.upgrade() else { return };
            let state = ui.global::<State>();
            let peer_id = state.get_selected_id().to_string();
            let peer_name = state.get_selected_name().to_string();
            if peer_id.is_empty() || state.get_selected_sending() {
                return;
            }
            let Some(node) = ctx.node() else {
                toast(&ui, "Still connecting. Try again in a moment.", false);
                return;
            };
            for send in take_composed(&ctx, &ui) {
                start_send(
                    &ctx,
                    &ui,
                    node.clone(),
                    peer_id.clone(),
                    peer_name.clone(),
                    send.paths,
                    send.staged,
                    send.cleanup,
                );
            }
        });
    }
    {
        // Fill the text box from the clipboard.
        let weak = ctx.weak.clone();
        logic.on_paste_clipboard(move || {
            let weak = weak.clone();
            platform::read_clipboard(move |text| {
                let Some(ui) = weak.upgrade() else { return };
                match text.filter(|t| !t.trim().is_empty()) {
                    Some(text) => ui.global::<State>().set_paste_input(text.into()),
                    None => toast(&ui, "The clipboard has no text", false),
                }
            });
        });
    }
    {
        // Drop one queued share-sheet file.
        let ctx = ctx.clone();
        logic.on_outbox_remove(move |path| {
            let path = PathBuf::from(path.as_str());
            let picked = {
                let mut picked = ctx.transfers.picked.lock().unwrap();
                let before = picked.len();
                picked.retain(|p| *p != path);
                picked.len() != before
            };
            if !picked && queued_staged(&ctx).contains(&path) {
                android::clear_outbox(&[path]);
            }
            refresh_outbox(&ctx);
        });
    }
    {
        // Drop the whole queue (files being sent stay).
        let ctx = ctx.clone();
        logic.on_outbox_clear(move || {
            ctx.transfers.picked.lock().unwrap().clear();
            android::clear_outbox(&queued_staged(&ctx));
            refresh_outbox(&ctx);
        });
    }

    // Desktop: attach picked files to the composer (Android has its own
    // pickers, which stage copies).
    #[cfg(not(target_os = "android"))]
    {
        let ctx = ctx.clone();
        logic.on_pick_files(move || {
            if ctx.transfers.picking.swap(true, Ordering::SeqCst) {
                return;
            }
            let ctx = ctx.clone();
            ctx.rt.clone().spawn_blocking(move || {
                let picked = platform::pick_send_paths();
                ctx.transfers.picking.store(false, Ordering::SeqCst);
                {
                    let mut list = ctx.transfers.picked.lock().unwrap();
                    for path in picked {
                        if !list.contains(&path) {
                            list.push(path);
                        }
                    }
                }
                refresh_outbox(&ctx);
            });
        });
    }

    // Desktop: pick files, share them and deliver them to the selected peer.
    // Android queues picks in the send card instead.
    #[cfg(not(target_os = "android"))]
    {
        let ctx = ctx.clone();
        logic.on_send_to_peer(move || pick_and_send(&ctx));
    }
    {
        let ctx = ctx.clone();
        logic.on_stop_share(move |key: SharedString| stop_send(&ctx, key.to_string()));
    }
    {
        let ctx = ctx.clone();
        logic.on_quick_send(move |peer_id: SharedString| quick_send(&ctx, &peer_id));
    }
    {
        let ctx = ctx.clone();
        logic.on_quick_accept(move |key: SharedString| answer_quick(&ctx, &key, true));
    }
    {
        let ctx = ctx.clone();
        logic.on_quick_decline(move |key: SharedString| answer_quick(&ctx, &key, false));
    }
    {
        let ctx = ctx.clone();
        logic.on_cancel_receive(move |key: SharedString| {
            let sender = ctx
                .transfers
                .recv_cancels
                .lock()
                .unwrap()
                .remove(key.as_str());
            if let Some(tx) = sender {
                let _ = tx.send(());
                if let Some(ui) = ctx.weak.upgrade() {
                    update_row(&ui, &key, |row| row.status = "Cancelling…".into());
                }
            }
        });
    }
    {
        let weak = ctx.weak.clone();
        logic.on_dismiss_transfer(move |key: SharedString| {
            let Some(ui) = weak.upgrade() else { return };
            let finished = with_rows(&ui, |rows| {
                position(rows, &key)
                    .and_then(|i| rows.row_data(i))
                    .is_some_and(|r| !r.active)
            });
            if finished == Some(true) {
                remove_row(&ui, &key);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invite_payload_is_parsed_and_normalised() {
        let v = serde_json::json!({
            "blob_ticket": "blobabc",
            "file_count": 3,
            "total_size": 1024,
            "sender_name": "Laptop",
            "remote_endpoint_id": "ABCDEF",
        });
        let invite = parse_invite_payload(&v);
        assert_eq!(invite.ticket, "blobabc");
        assert_eq!((invite.file_count, invite.total_size), (3, 1024));
        assert_eq!(invite.sender_name, "Laptop");
        assert_eq!(invite.peer_id, "abcdef");
    }

    #[test]
    fn invite_payload_missing_fields_are_empty() {
        let invite = parse_invite_payload(&serde_json::json!({}));
        assert!(invite.ticket.is_empty() && invite.peer_id.is_empty());
        assert_eq!(invite.file_count, 0);
    }

    #[test]
    fn invite_peer_name_prefers_stored_then_claimed_then_short_id() {
        let id = "0123456789abcdef";
        assert_eq!(invite_peer_name(Some("Desk".into()), "Laptop", id), "Desk");
        assert_eq!(invite_peer_name(Some(" ".into()), "Laptop", id), "Laptop");
        assert_eq!(invite_peer_name(Some(id.into()), "", id), "01234567");
        assert_eq!(invite_peer_name(None, id, id), "01234567");
    }

    #[test]
    fn items_title_singular_and_plural() {
        assert_eq!(items_title("a.txt", 1), "a.txt");
        assert_eq!(items_title("a.txt", 4), "4 items");
    }

    #[test]
    fn paste_title_and_links() {
        assert_eq!(items_title(PASTE_FILE_NAME, 1), "text");
        assert_eq!(
            as_link("  https://example.com/a?b=c \n"),
            Some("https://example.com/a?b=c")
        );
        assert_eq!(as_link("see https://example.com"), None);
        assert_eq!(as_link("https://"), None);
        assert_eq!(as_link("ftp://example.com"), None);
    }

    #[test]
    fn paste_preview_is_one_short_line() {
        assert_eq!(text_preview("  hello\n\n  world\t! "), "hello world !");
        let long = "é".repeat(PREVIEW_MAX_CHARS + 5);
        let preview = text_preview(&long);
        assert_eq!(preview.chars().count(), PREVIEW_MAX_CHARS + 1);
        assert!(preview.ends_with('…'));
        assert_eq!(text_preview(&"x".repeat(PREVIEW_MAX_CHARS)).chars().count(), PREVIEW_MAX_CHARS);
    }

    #[test]
    fn received_paste_is_read_from_its_saved_name() {
        let dir = std::env::temp_dir().join(format!("tm-slint-paste-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let renamed = dir.join("flipflop-paste (1).txt");
        std::fs::write(&renamed, "hello").unwrap();

        let mut received = Received::default();
        received.note("receive-file-names", Some(r#"["flipflop-paste.txt"]"#));
        let conflicts = serde_json::json!([{
            "original": dir.join(PASTE_FILE_NAME),
            "resolved": renamed,
        }]);
        received.note("receive-conflicts", Some(&conflicts.to_string()));
        assert_eq!(received.pasted_text(&dir).as_deref(), Some("hello"));

        // Anything else is just files.
        received.note("receive-file-names", Some(r#"["notes.txt"]"#));
        assert_eq!(received.pasted_text(&dir), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn partial_store_path_rejects_garbage() {
        assert!(partial_store_path("not a ticket").is_none());
    }

    #[test]
    fn sizes_and_types_of_picked_paths() {
        let dir = std::env::temp_dir().join(format!("tm-slint-paths-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a"), [0u8; 10]).unwrap();
        std::fs::write(dir.join("sub/b"), [0u8; 5]).unwrap();

        assert_eq!(dir_size(&dir), 15);
        assert_eq!(dir_size(&dir.join("a")), 10);
        assert_eq!(dir_size(&dir.join("missing")), 0);
        assert!(matches!(
            path_type_of(std::slice::from_ref(&dir)),
            Some(crate::engine::TransferPathType::Directory)
        ));
        let both = [dir.join("a"), dir.join("sub")];
        assert!(path_type_of(&both).is_none());
        let meta = metadata_for(&both);
        assert_eq!((meta.item_count, meta.size), (2, 15));
        assert_eq!(
            meta.mime_type.as_deref(),
            Some("application/x-iroh-collection")
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
