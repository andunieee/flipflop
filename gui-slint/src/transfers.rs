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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use engine::{
    resolve_relay_mode_with_fallback, sanitize_folder_name, start_share_items, AddrInfoOptions,
    AppHandle, DiscoveryModeOption, EventEmitter, NodeService, PairedDeviceInfo, ReceiveOptions,
    RelayModeOption, SendOptions, TransferDirection, TransferPeer, TransferStatus,
};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel, Weak};

use crate::app::{refresh_history, short_id, AppCtx};
use crate::emitter::apply_transfer_event;
use crate::format::fmt_bytes;
use crate::platform::{self, toast, toast_later};
use crate::recorder::{Ctx, Recorder};
use crate::settings::Settings;
use crate::{android, AppWindow, Logic, State, TransferRow};

/// Cross-thread transfer bookkeeping, shared through `AppCtx`.
#[derive(Clone, Default)]
pub struct Transfers {
    shares: Arc<tokio::sync::Mutex<Shares>>,
    /// Cancel senders of running receives, by key. A receive whose sender is
    /// gone when it ends was cancelled by the user.
    recv_cancels: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>>,
    next_key: Arc<AtomicU64>,
    /// A file picker is open (ignore further "Send files…" clicks).
    picking: Arc<AtomicBool>,
}

#[derive(Default)]
struct Shares {
    open: HashMap<String, ShareHandle>,
    /// Sends stopped while their share was still being set up; the setup
    /// task shuts the share down instead of registering it.
    stopped: HashSet<String>,
}

impl Transfers {
    fn new_key(&self, prefix: &str) -> String {
        format!("{prefix}-{}", self.next_key.fetch_add(1, Ordering::Relaxed))
    }
}

struct ShareHandle {
    send_result: engine::SendResult,
    recorder: Arc<Recorder>,
    peer_name: String,
    /// Android share-sheet files this share serves; deleted once no open
    /// share needs them any more.
    staged: Vec<PathBuf>,
}

impl ShareHandle {
    async fn stop(&self) {
        match tokio::time::timeout(Duration::from_secs(2), self.send_result.router.shutdown()).await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("router shutdown error: {e}"),
            Err(_) => tracing::warn!("router shutdown timed out after 2s"),
        }
        self.send_result.router.endpoint().close().await;
    }
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

fn insert_row(ui: &AppWindow, row: TransferRow) {
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

fn update_row(ui: &AppWindow, key: &str, f: impl FnOnce(&mut TransferRow)) {
    with_rows(ui, |rows| {
        if let Some(i) = position(rows, key) {
            let mut row = rows.row_data(i).expect("index in range");
            f(&mut row);
            rows.set_row_data(i, row);
        }
    });
    sync_peer_activity(ui);
}

fn remove_row(ui: &AppWindow, key: &str) {
    with_rows(ui, |rows| {
        if let Some(i) = position(rows, key) {
            rows.remove(i);
        }
    });
    sync_peer_activity(ui);
}

/// `update_row` from any thread.
fn post_update(
    weak: &Weak<AppWindow>,
    key: &str,
    f: impl FnOnce(&mut TransferRow) + Send + 'static,
) {
    let key = key.to_string();
    let _ = weak.upgrade_in_event_loop(move |ui| update_row(&ui, &key, f));
}

/// Mark a row as finished with an error.
fn fail(weak: &Weak<AppWindow>, key: &str, msg: String) {
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
        .filter(|t| t.active)
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
    let selected = state.get_selected_id();
    state.set_selected_sending(busy(&selected, true));

    // The selected peer's rows. Update in place when the same transfers are
    // shown so cards (and their progress animation) aren't rebuilt.
    let visible: Vec<TransferRow> = state
        .get_transfers()
        .iter()
        .filter(|t| t.peer_id == selected)
        .collect();
    let shown = state.get_visible_transfers();
    let same_rows = shown.row_count() == visible.len()
        && visible
            .iter()
            .enumerate()
            .all(|(i, t)| shown.row_data(i).is_some_and(|s| s.key == t.key));
    if same_rows {
        for (i, row) in visible.into_iter().enumerate() {
            if shown.row_data(i).as_ref() != Some(&row) {
                shown.set_row_data(i, row);
            }
        }
    } else {
        state.set_visible_transfers(ModelRc::from(Rc::new(VecModel::from(visible))));
    }
}

// ------------------------------------------------------------ engine events

struct TransferEmitter {
    weak: Weak<AppWindow>,
    key: String,
    recorder: Arc<Recorder>,
    /// Sends: closes the share once the peer has everything.
    close_when_sent: Option<AppCtx>,
}

impl TransferEmitter {
    fn emit(&self, name: &str, payload: Option<&str>) {
        self.recorder.note(name, payload);
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

/// Resolve the configured relay (with public fallback when selected) and
/// discovery mode for a transfer. Mirrors the Tauri shell: custom relays are
/// probed, and a strict-but-unreachable relay fails the transfer.
async fn resolve_network(
    settings: &Arc<Mutex<Settings>>,
) -> Result<(RelayModeOption, DiscoveryModeOption), String> {
    let (arg, discovery_mode) = {
        let guard = settings.lock().unwrap();
        (guard.relay_config_arg(), guard.discovery_mode())
    };
    let (relay_mode, fell_back) = resolve_relay_mode_with_fallback(Some(arg)).await?;
    if fell_back {
        tracing::warn!("custom relay unreachable; fell back to public relays");
    }
    Ok((relay_mode, discovery_mode))
}

// --------------------------------------------------------------------- send

fn path_name(path: &Path) -> String {
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

fn path_type_of(paths: &[PathBuf]) -> Option<engine::TransferPathType> {
    match paths {
        [only] if only.is_dir() => Some(engine::TransferPathType::Directory),
        [_] => Some(engine::TransferPathType::File),
        _ => None,
    }
}

fn metadata_for(paths: &[PathBuf]) -> engine::FileMetadata {
    let mime_type = match paths {
        [only] if only.is_dir() => "inode/directory",
        [_] => "application/octet-stream",
        _ => "application/x-iroh-collection",
    };
    engine::FileMetadata {
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
fn items_title(first_name: &str, count: usize) -> String {
    if count == 1 {
        first_name.to_string()
    } else {
        format!("{count} items")
    }
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
) {
    let key = ctx.transfers.new_key("send");
    tracing::info!(%peer_id, %key, paths = paths.len(), "send: starting flow");
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

    let ctx = ctx.clone();
    let weak = ctx.weak.clone();
    ctx.rt.clone().spawn(async move {
        // Walking folders for their size can take a while: keep it off the
        // async workers.
        let scan = paths.clone();
        let (metadata, path_type) =
            match tokio::task::spawn_blocking(move || (metadata_for(&scan), path_type_of(&scan)))
                .await
            {
                Ok(v) => v,
                Err(e) => return fail(&weak, &key, format!("Could not read the files: {e}")),
            };
        let (byte_count, item_count) = (metadata.size, metadata.item_count);
        post_update(&weak, &key, move |row| {
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
                    endpoint_id: peer_id.clone(),
                    display_name: Some(peer_name.clone()),
                    device_type: None,
                }),
                ..Ctx::default()
            },
            history_enabled,
        ));
        let emitter = TransferEmitter {
            weak: weak.clone(),
            key: key.clone(),
            recorder: recorder.clone(),
            close_when_sent: Some(ctx.clone()),
        };
        let app_handle: AppHandle = Some(Arc::new(emitter));

        let (relay_mode, discovery_mode) = match resolve_network(&ctx.settings).await {
            Ok(v) => v,
            Err(e) => {
                forget_stopped(&ctx, &key).await;
                return fail(&weak, &key, format!("Could not configure network: {e}"));
            }
        };
        let options = SendOptions {
            relay_mode,
            discovery_mode,
            ticket_type: AddrInfoOptions::RelayAndAddresses,
            magic_ipv4_addr: None,
            magic_ipv6_addr: None,
        };
        let share = match tokio::time::timeout(
            Duration::from_secs(60),
            start_share_items(paths, options, &app_handle, Some(metadata)),
        )
        .await
        {
            Ok(Ok(share)) => share,
            failed => {
                let error = match failed {
                    Ok(Err(e)) => format!("{e:#}"),
                    _ => "timed out".to_string(),
                };
                tracing::warn!(%peer_id, "send: share setup failed: {error}");
                forget_stopped(&ctx, &key).await;
                return fail(&weak, &key, format!("Could not start share: {error}"));
            }
        };
        let ticket = share.ticket.clone();
        let handle = ShareHandle {
            send_result: share,
            recorder,
            peer_name: peer_name.clone(),
            staged,
        };
        {
            let mut shares = ctx.transfers.shares.lock().await;
            if shares.stopped.remove(&key) {
                // Stopped while preparing: the row is already gone.
                drop(shares);
                shutdown_share(&ctx, handle).await;
                return;
            }
            shares.open.insert(key.clone(), handle);
        }

        post_update(&weak, &key, |row| row.status = "Contacting peer…".into());
        let delivered = match node
            .invite_paired_device(&peer_id, &ticket, item_count, byte_count)
            .await
        {
            Ok(delivered) => delivered,
            Err(e) => {
                tracing::warn!(%peer_id, "send: invite delivery failed: {e:#}");
                false
            }
        };
        if delivered {
            tracing::info!(%peer_id, "send: invite delivered, waiting for peer to pull");
            let status = format!("Delivered — waiting for {peer_name} to download…");
            post_update(&weak, &key, move |row| {
                // Progress events may already have moved the status on.
                if row.status.as_str() == "Contacting peer…" {
                    row.status = status.into();
                }
            });
        } else {
            tracing::warn!(%peer_id, "send: invite not delivered (peer unreachable)");
            // Nobody else knows the ticket: the share is useless.
            if let Some(handle) = take_share(&ctx, &key).await {
                shutdown_share(&ctx, handle).await;
                fail(
                    &weak,
                    &key,
                    format!("Could not reach {peer_name}. Are they online?"),
                );
            }
        }
    });
}

async fn take_share(ctx: &AppCtx, key: &str) -> Option<ShareHandle> {
    ctx.transfers.shares.lock().await.open.remove(key)
}

async fn forget_stopped(ctx: &AppCtx, key: &str) {
    ctx.transfers.shares.lock().await.stopped.remove(key);
}

/// Close the share's endpoint and release share-sheet files nothing else uses.
async fn shutdown_share(ctx: &AppCtx, handle: ShareHandle) {
    handle.stop().await;
    if handle.staged.is_empty() {
        return;
    }
    let unused: Vec<PathBuf> = {
        let shares = ctx.transfers.shares.lock().await;
        handle
            .staged
            .iter()
            .filter(|p| !shares.open.values().any(|h| h.staged.contains(p)))
            .cloned()
            .collect()
    };
    android::clear_outbox(&unused);
    let _ = ctx.weak.upgrade_in_event_loop(|ui| {
        ui.global::<State>()
            .set_outbox_count(android::outbox_files().len() as i32);
    });
}

/// The peer has the files: close the share and settle the row.
async fn finish_send(ctx: AppCtx, key: String) {
    // Let the last acknowledgements flush before tearing the endpoint down.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let Some(handle) = take_share(&ctx, &key).await else {
        return;
    };
    let peer_name = handle.peer_name.clone();
    shutdown_share(&ctx, handle).await;
    let ctx_ui = ctx.clone();
    let _ = ctx.weak.upgrade_in_event_loop(move |ui| {
        update_row(&ui, &key, |row| {
            row.active = false;
            row.status = format!("Sent to {peer_name}").into();
        });
        refresh_history(&ctx_ui);
    });
}

/// User pressed Stop on a send.
fn stop_send(ctx: &AppCtx, key: String) {
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
        if let Some(handle) = handle {
            // Finalize first so the abort's own "transfer-failed" can't win.
            handle
                .recorder
                .finalize(TransferStatus::Cancelled, None, None, None, None);
            shutdown_share(&ctx, handle).await;
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
        engine::storage::RECV_DIR_PREFIX,
        ticket.hash().to_hex()
    );
    Some(
        engine::storage::temp_dir()
            .join(dir)
            .to_string_lossy()
            .into_owned(),
    )
}

/// A paired peer sent us files: accept and download them, no prompt.
/// UI thread (called from node event routing).
pub(crate) fn accept_invite(ctx: &AppCtx, payload: serde_json::Value) {
    let invite = parse_invite_payload(&payload);
    if invite.peer_id.is_empty() || invite.ticket.is_empty() {
        return;
    }
    let Some(node) = ctx.node() else {
        return;
    };
    let Some(ui) = ctx.weak.upgrade() else {
        return;
    };

    let key = ctx.transfers.new_key("recv");
    let items = if invite.file_count == 1 {
        "1 item".to_string()
    } else {
        format!("{} items", invite.file_count)
    };
    insert_row(
        &ui,
        TransferRow {
            key: key.clone().into(),
            peer_id: invite.peer_id.clone().into(),
            sending: false,
            active: true,
            title: items.into(),
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
        tracing::info!(%id, %key, "receive: auto-accepting invite");
        let stored_name = node
            .list_paired()
            .ok()
            .and_then(|list: Vec<PairedDeviceInfo>| {
                list.into_iter()
                    .find(|d| d.endpoint_id.to_lowercase() == id)
                    .map(|d| d.display_name)
            });
        let peer_name = invite_peer_name(stored_name, &invite.sender_name, &id);
        let (save_dir, history_enabled) = {
            let settings = ctx.settings.lock().unwrap();
            let save_dir = settings
                .downloads_path()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(sanitize_folder_name(&peer_name, &short_id(&id)));
            (save_dir, settings.history_enabled)
        };
        if let Err(e) = std::fs::create_dir_all(&save_dir) {
            tracing::warn!(save = %save_dir.display(), "receive: cannot create save dir: {e}");
        }

        let give_up = |msg: String| {
            ctx.transfers.recv_cancels.lock().unwrap().remove(&key);
            toast_later(&weak, msg.clone(), true);
            fail(&weak, &key, msg);
        };

        if let Err(e) = node.respond_paired_invite(&id, true).await {
            tracing::warn!(%id, "receive: accept handshake failed: {e}");
            return give_up(format!("Could not accept files from {peer_name}: {e}"));
        }
        let (relay_mode, discovery_mode) = match resolve_network(&ctx.settings).await {
            Ok(v) => v,
            Err(e) => return give_up(format!("Could not configure network: {e}")),
        };
        let options = ReceiveOptions {
            output_dir: Some(save_dir.clone()),
            relay_mode,
            discovery_mode,
            magic_ipv4_addr: None,
            magic_ipv6_addr: None,
        };
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
        let app_handle: AppHandle = Some(Arc::new(TransferEmitter {
            weak: weak.clone(),
            key: key.clone(),
            recorder: recorder.clone(),
            close_when_sent: None,
        }));
        toast_later(&weak, format!("{peer_name} is sending you files"), false);
        post_update(&weak, &key, |row| row.status = "Connecting…".into());

        let result = engine::download(invite.ticket, options, app_handle, cancel_rx).await;
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
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    update_row(&ui, &key, |row| row.active = false);
                    toast(&ui, &format!("Files from {peer_name} saved"), false);
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

// ---------------------------------------------------------------- callbacks

pub(crate) fn register(ctx: &AppCtx) {
    let Some(ui) = ctx.weak.upgrade() else {
        return;
    };
    let logic = ui.global::<Logic>();

    {
        // Pick files, share them and deliver to the selected peer.
        let ctx = ctx.clone();
        logic.on_send_to_peer(move || {
            let Some(ui) = ctx.weak.upgrade() else { return };
            let Some(node) = ctx.node() else {
                toast(&ui, "Still connecting — try again in a moment.", false);
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
                    // Touch devices pick content through the system share
                    // sheet; say so instead of silently doing nothing.
                    if platform::TOUCH {
                        toast_later(
                            &ctx.weak,
                            "Use the system share sheet (\"Send to TunnelManager\") to stage files, then pick a peer here.",
                            false,
                        );
                    }
                    return;
                }
                let staged = if cfg!(target_os = "android") {
                    picked.clone()
                } else {
                    Vec::new()
                };
                let ctx_ui = ctx.clone();
                let _ = ctx.weak.upgrade_in_event_loop(move |ui| {
                    start_send(&ctx_ui, &ui, node, peer_id, peer_name, picked, staged);
                });
            });
        });
    }
    {
        let ctx = ctx.clone();
        logic.on_stop_share(move |key: SharedString| stop_send(&ctx, key.to_string()));
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
            Some(engine::TransferPathType::Directory)
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
