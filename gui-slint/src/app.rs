use crate::android;
use crate::emitter::{MainEmitter, MainQueue, ReceiveCancels};
use crate::format;
use crate::recorder::{Ctx, Recorder};
use crate::settings::Settings;
use crate::{AppWindow, HistoryRow, Logic, PeerRow, State};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel, Weak};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use engine::{
    get_relay_status, is_reclaimable_partial, reclaim_partial, resolve_relay_mode_with_fallback,
    sanitize_folder_name, start_share_items, verify_discovery, verify_relays, AddrInfoOptions,
    AppHandle, DiscoveryConfigArg, DiscoveryModeOption, EventEmitter, NodeService, PairedDeviceInfo,
    ReceiveOptions, RelayConfigArg, SendOptions, TransferDirection, TransferHistoryStore,
    TransferPeer, TransferRecord, TransferStatus,
};

/// Display geometry for the window: desktop defaults, or phone-friendly
/// sizes on touch devices.
#[cfg(target_os = "android")]
const WINDOW_TOUCH: bool = true;
#[cfg(not(target_os = "android"))]
const WINDOW_TOUCH: bool = false;

type PeerMetaMap = HashMap<String, (String, String, bool)>;

/// All state that is shared across threads. Cloned freely into async tasks.
#[derive(Clone)]
struct Sync {
    weak: Weak<AppWindow>,
    rt: tokio::runtime::Handle,
    share: Arc<tokio::sync::Mutex<Option<ShareHandle>>>,
    history: Arc<TransferHistoryStore>,
    settings: Arc<Mutex<Settings>>,
    settings_path: PathBuf,
    node: Arc<Mutex<Option<Arc<NodeService>>>>,
    peer_meta: Arc<Mutex<PeerMetaMap>>,
    pair_requests: Arc<Mutex<Vec<(String, String)>>>,
    recv_cancels: ReceiveCancels,
    queue: MainQueue,
    /// Android: files staged from the system share sheet for the active
    /// send; removed from disk once the share ends.
    outbox: Arc<Mutex<Vec<PathBuf>>>,
}

struct ShareHandle {
    send_result: engine::SendResult,
    recorder: Option<Arc<Recorder>>,
}

impl ShareHandle {
    async fn stop(&self) {
        match tokio::time::timeout(
            std::time::Duration::from_secs(2),
            self.send_result.router.shutdown(),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("router shutdown error: {e}"),
            Err(_) => tracing::warn!("router shutdown timed out after 2s"),
        }
        self.send_result.router.endpoint().close().await;
    }
}

// ------------------------------------------------------- platform services
//
// Clipboard, native dialogs and "open file" are provided per-platform: JNI
// on Android (see android.rs), rfd/arboard/open on desktop.

#[cfg(target_os = "android")]
fn copy_to_clipboard(text: &str) -> Result<(), String> {
    android::copy_to_clipboard(text)
}

#[cfg(not(target_os = "android"))]
fn copy_to_clipboard(text: &str) -> Result<(), String> {
    use std::cell::RefCell;

    thread_local! {
        // One clipboard per UI thread, kept alive for the whole run so X11
        // clipboard managers always see the contents (arboard warns when the
        // Clipboard is dropped immediately after writing).
        static CLIPBOARD: RefCell<Option<arboard::Clipboard>> = const { RefCell::new(None) };
    }

    CLIPBOARD.with_borrow_mut(|slot: &mut Option<arboard::Clipboard>| {
        if slot
            .as_mut()
            .map(|cb| cb.set_text(text).is_ok())
            .unwrap_or(false)
        {
            return Ok(());
        }
        // (Re)create the clipboard and write again.
        let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
        let result = cb.set_text(text.to_string());
        *slot = Some(cb);
        result.map_err(|e| e.to_string())
    })
}

fn toast(_ui: &AppWindow, msg: &str, error: bool) {
    // Native toast/snackbar; the UI itself stays untouched.
    android::show_toast(msg, error);
}

/// Files/folders the user wants to send. Empty = cancelled.
#[cfg(target_os = "android")]
fn pick_send_paths() -> Vec<PathBuf> {
    // Android has no native file-picker result plumbing, so sending means
    // staging via the system share sheet ("Send to TunnelManager"); this
    // returns the launch intent's content plus everything already staged.
    android::pick_send_files()
}

#[cfg(not(target_os = "android"))]
fn pick_send_paths() -> Vec<PathBuf> {
    // Try the multi-file picker first; if nothing was picked, offer the
    // folder picker as a second step (cancelled folder pick => empty vec).
    rfd::FileDialog::new()
        .set_title("Choose files to send")
        .pick_files()
        .unwrap_or_else(|| {
            rfd::FileDialog::new()
                .set_title("Or pick a folder")
                .pick_folder()
                .map(|f| vec![f])
                .unwrap_or_default()
        })
}

/// Folder to store received files in. `None` keeps the default.
#[cfg(target_os = "android")]
fn pick_downloads_folder() -> Option<PathBuf> {
    // Android: keep the engine default (app-private Downloads dir); the
    // SAF directory picker needs activity-result plumbing that
    // android-activity does not expose.
    None
}

#[cfg(not(target_os = "android"))]
fn pick_downloads_folder() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Choose downloads folder")
        .pick_folder()
}

/// Hand a saved file back to the OS (file manager / viewer).
#[cfg(target_os = "android")]
fn open_path(_path: &str) {
    // No-op: surfacing received files is done through the system
    // Downloads/Files app on Android.
}

#[cfg(not(target_os = "android"))]
fn open_path(path: &str) {
    if let Err(e) = open::that(path) {
        tracing::warn!("failed to open {path:?}: {e}");
    }
}

fn path_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn dir_size(path: &std::path::Path) -> u64 {
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
            let path = entry.path();
            let Ok(md) = entry.metadata() else { continue };
            if md.is_dir() {
                stack.push(path);
            } else {
                total = total.saturating_add(md.len());
            }
        }
    }
    total
}

fn path_type_of(paths: &[PathBuf]) -> Option<engine::TransferPathType> {
    if paths.len() > 1 {
        None
    } else if paths.first().map(|p| p.is_dir()).unwrap_or(false) {
        Some(engine::TransferPathType::Directory)
    } else {
        Some(engine::TransferPathType::File)
    }
}

fn metadata_for(paths: &[PathBuf]) -> engine::FileMetadata {
    let size = paths.iter().map(|p| dir_size(p)).sum();
    let file_name = paths
        .first()
        .map(|p| path_name(p))
        .unwrap_or_else(|| "share".to_string());
    let mime_type = if paths.len() > 1 {
        Some("application/x-iroh-collection".to_string())
    } else if paths.first().map(|p| p.is_dir()).unwrap_or(false) {
        Some("inode/directory".to_string())
    } else {
        Some("application/octet-stream".to_string())
    };
    engine::FileMetadata {
        file_name,
        item_count: paths.len() as u32,
        size,
        thumbnail: None,
        mime_type,
        items: None,
    }
}

fn short_id(endpoint_id: &str) -> String {
    endpoint_id.chars().take(8).collect()
}

fn status_text(status: TransferStatus) -> &'static str {
    match status {
        TransferStatus::InProgress => "In progress",
        TransferStatus::Completed => "Completed",
        TransferStatus::Failed => "Failed",
        TransferStatus::Cancelled => "Cancelled",
        TransferStatus::Interrupted => "Interrupted",
    }
}

fn row_from_record(record: &TransferRecord) -> HistoryRow {
    let title = if !record.root_name.is_empty() {
        record.root_name.clone()
    } else {
        record
            .file_names
            .first()
            .cloned()
            .unwrap_or_else(|| "Transfer".to_string())
    };
    let mut detail = String::new();
    if record.item_count > 1 {
        detail.push_str(&format!("{} items", record.item_count));
    } else if let Some(name) = record.file_names.first() {
        detail.push_str(name);
    }
    if let Some(path) = record.save_path.as_deref() {
        if !detail.is_empty() {
            detail.push_str(" · ");
        }
        detail.push_str(path);
    }
    if record.conflict_count > 0 {
        detail.push_str(&format!(" · {} renamed", record.conflict_count));
    }
    let failed = matches!(record.status, TransferStatus::Failed);

    HistoryRow {
        id: record.id.clone().into(),
        title: title.into(),
        direction: match record.direction {
            TransferDirection::Send => "send",
            TransferDirection::Receive => "receive",
        }
        .into(),
        status: status_text(record.status).into(),
        detail: detail.into(),
        date: format::fmt_date(record.started_at).into(),
        size: format::fmt_bytes(record.payload_bytes).into(),
        speed: record
            .avg_speed_bps
            .map(format::fmt_speed)
            .unwrap_or_else(|| "—".to_string())
            .into(),
        can_open: record.save_path.is_some(),
        failed,
    }
}

fn paired_row(d: &PairedDeviceInfo) -> PeerRow {
    let name = if d.display_name.trim().is_empty() {
        short_id(&d.endpoint_id)
    } else {
        d.display_name.clone()
    };
    let mut detail = d.device_type.clone();
    if !d.os.trim().is_empty() {
        detail.push_str(" · ");
        detail.push_str(&d.os);
    }
    detail.push_str(" · ");
    detail.push_str(&short_id(&d.endpoint_id));
    PeerRow {
        endpoint_id: d.endpoint_id.clone().into(),
        name: name.into(),
        detail: detail.into(),
        online: d.online,
        is_request: false,
        is_suggestion: false,
        trusted: d.trusted,
    }
}

fn request_row(id: &str, name: &str) -> PeerRow {
    PeerRow {
        endpoint_id: id.to_string().into(),
        name: name.to_string().into(),
        detail: "wants to pair with you".into(),
        online: true,
        is_request: true,
        is_suggestion: false,
        trusted: false,
    }
}

fn nearby_row(n: &engine::NearbyDevice) -> PeerRow {
    let id = n.endpoint_id.to_lowercase();
    let name = match &n.display_name {
        Some(name) if !name.trim().is_empty() => name.clone(),
        _ if !n.fingerprint.trim().is_empty() => n.fingerprint.clone(),
        _ => short_id(&n.endpoint_id),
    };
    PeerRow {
        endpoint_id: id.into(),
        name: name.into(),
        detail: if n.identified {
            "Found on your local network".into()
        } else {
            "On your local network (unidentified)".into()
        },
        online: true,
        is_request: false,
        is_suggestion: true,
        trusted: false,
    }
}

// -------------------------------------------------------- ui refreshers

fn sync_meta(devices: &[PairedDeviceInfo], meta: &Arc<Mutex<PeerMetaMap>>) {
    let mut guard = meta.lock().unwrap();
    guard.clear();
    for d in devices {
        let id = d.endpoint_id.to_lowercase();
        let name = if d.display_name.trim().is_empty() {
            short_id(&d.endpoint_id)
        } else {
            d.display_name.clone()
        };
        let detail = format!(
            "{} · {} · {}",
            if d.device_type.is_empty() {
                "device".to_string()
            } else {
                d.device_type.clone()
            },
            if d.os.trim().is_empty() {
                "unknown os".to_string()
            } else {
                d.os.clone()
            },
            short_id(&d.endpoint_id),
        );
        guard.insert(id, (name, detail, d.online));
    }
}

fn refresh_peers(sync: &Sync) {
    let Some(node) = sync.node.lock().unwrap().clone() else {
        return;
    };
    let devices = node.list_paired().unwrap_or_default();
    sync_meta(&devices, &sync.peer_meta);

    let weak = sync.weak.clone();
    let sync = sync.clone();
    let _ = slint::invoke_from_event_loop(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let state = ui.global::<State>();
        let rows: Vec<PeerRow> = devices.iter().map(paired_row).collect();
        let online_count = devices.iter().filter(|d| d.online).count();
        state.set_peers(ModelRc::from(Rc::new(VecModel::from(rows))));
        state.set_presence_label(format!("{}/{} peers online", online_count, devices.len()).into());
        let info = node.device_info();
        state.set_my_name(info.display_name.into());
        state.set_node_ready(node.is_network_ready());
        match node.pairing_ticket() {
            Ok(ticket) => state.set_my_ticket(ticket.into()),
            Err(e) => tracing::debug!("pairing_ticket unavailable: {e}"),
        }
        if state.get_selected_id().is_empty() {
            if let Some(first) = devices.first() {
                state.set_selected_id(first.endpoint_id.clone().into());
            }
        }
        drop(state);
        if ui.global::<State>().get_page() == "peer" {
            refresh_history(&sync);
        }
    });
}

fn refresh_suggestions(sync: &Sync) {
    let Some(node) = sync.node.lock().unwrap().clone() else {
        return;
    };
    let weak = sync.weak.clone();
    let requests = sync.pair_requests.lock().unwrap().clone();
    sync.rt.spawn(async move {
        let nearby = node.list_nearby().await;
        let reason = node.nearby_unavailable_reason();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let state = ui.global::<State>();
            let mut rows: Vec<PeerRow> = Vec::new();
            for (id, name) in &requests {
                rows.push(request_row(id, name));
            }
            for d in &nearby {
                let id = d.endpoint_id.to_lowercase();
                if requests.iter().any(|(i, _)| i == &id) {
                    continue;
                }
                rows.push(nearby_row(d));
            }
            state.set_suggestions(ModelRc::from(Rc::new(VecModel::from(rows))));
            state.set_nearby_note(reason.unwrap_or_default().into());
        });
    });
}

fn refresh_history(sync: &Sync) {
    let selected = sync
        .weak
        .upgrade()
        .map(|ui| {
            ui.global::<State>()
                .get_selected_id()
                .to_string()
                .to_lowercase()
        })
        .unwrap_or_default();
    let history = sync.history.clone();
    let weak = sync.weak.clone();
    sync.rt.spawn_blocking(move || {
        let rows: Vec<HistoryRow> = history
            .list()
            .map(|records| {
                records
                    .iter()
                    .filter(|r| {
                        !selected.is_empty()
                            && r.peer
                                .as_ref()
                                .map(|p| p.endpoint_id.to_lowercase() == selected)
                                .unwrap_or(false)
                    })
                    .rev()
                    .map(row_from_record)
                    .collect()
            })
            .unwrap_or_default();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() {
                ui.global::<State>()
                    .set_history(ModelRc::from(Rc::new(VecModel::from(rows))));
            }
        });
    });
}

fn clear_receive_ui_for(sync: &Sync, peer_id: &str) {
    let Some(ui) = sync.weak.upgrade() else {
        return;
    };
    let state = ui.global::<State>();
    if state.get_receive_active_id().to_string().to_lowercase() == peer_id {
        state.set_receive_active_id("".into());
        state.set_receive_status("".into());
        state.set_receive_error("".into());
        state.set_receive_progress(0.0);
        state.set_receive_progress_label("".into());
        state.set_receive_speed("".into());
    }
}

// ------------------------------------------------------------- transfers

struct TransferEmitter {
    weak: Weak<AppWindow>,
    recorder: Arc<Recorder>,
}

impl engine::EventEmitter for TransferEmitter {
    fn emit_event(&self, event_name: &str) -> Result<(), String> {
        self.recorder.note(event_name, None);
        let weak = self.weak.clone();
        let name = event_name.to_string();
        let _ = slint::invoke_from_event_loop(move || {
            crate::emitter::handle_event(&weak, &name, None);
        });
        Ok(())
    }

    fn emit_event_with_payload(&self, event_name: &str, payload: &str) -> Result<(), String> {
        self.recorder.note(event_name, Some(payload));
        let weak = self.weak.clone();
        let name = event_name.to_string();
        let payload = payload.to_string();
        let _ = slint::invoke_from_event_loop(move || {
            crate::emitter::handle_event(&weak, &name, Some(payload.as_str()));
        });
        Ok(())
    }
}

/// Resolve the configured relay (with public fallback when selected) and
/// discovery mode for a transfer. Mirrors the Tauri shell: custom relays are
/// probed, and a strict-but-unreachable relay fails the transfer.
async fn resolve_network(
    settings: &Arc<Mutex<Settings>>,
) -> Result<(engine::RelayModeOption, DiscoveryModeOption), String> {
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

/// Build a `RelayConfigArg` from the current settings-page fields.
fn relay_arg_from_state(ui: &AppWindow) -> RelayConfigArg {
    let state = ui.global::<State>();
    let arg = RelayConfigArg {
        mode: match state.get_relay_mode() {
            1 => "disabled".to_string(),
            2 => "custom".to_string(),
            _ => "default".to_string(),
        },
        urls: state
            .get_relay_urls()
            .to_string()
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect(),
        auth_token: {
            let token = state.get_relay_token().to_string();
            (!token.trim().is_empty()).then_some(token)
        },
        fallback: Some(match state.get_relay_fallback() {
            1 => "public".to_string(),
            _ => "strict".to_string(),
        }),
    };
    drop(state);
    arg
}

/// Build a `DiscoveryConfigArg` from the current settings-page fields.
fn discovery_arg_from_state(ui: &AppWindow) -> DiscoveryConfigArg {
    let state = ui.global::<State>();
    let arg = DiscoveryConfigArg {
        mode: match state.get_discovery_mode() {
            1 => "custom".to_string(),
            _ => "default".to_string(),
        },
        pkarr_relay_url: {
            let url = state.get_discovery_pkarr_url().to_string();
            (!url.trim().is_empty()).then_some(url)
        },
        dns_origin: {
            let origin = state.get_discovery_dns_origin().to_string();
            (!origin.trim().is_empty()).then_some(origin)
        },
    };
    drop(state);
    arg
}

fn start_send(sync: Sync, node: Arc<NodeService>, peer_id: String, peer_name: String, paths: Vec<PathBuf>) {
    let weak = sync.weak.clone();
    let active_id = peer_id.clone();
    let _ = slint::invoke_from_event_loop(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let state = ui.global::<State>();
        state.set_send_active_id(active_id.into());
        state.set_share_starting(true);
        state.set_send_status("Preparing…".into());
        state.set_send_error("".into());
        state.set_send_progress(0.0);
        state.set_send_progress_label("".into());
        state.set_send_peers(0);
    });

    let rt = sync.rt.clone();
    let weak = sync.weak.clone();
    let sync_bg = sync.clone();

    rt.spawn(async move {
        let metadata = metadata_for(&paths);
        let path_type = path_type_of(&paths);
        let byte_count = metadata.size;
        let item_count = metadata.item_count;
        let recorder = Arc::new(Recorder::new(
            sync_bg.history.clone(),
            TransferDirection::Send,
            Ctx {
                root_name: metadata.file_name.clone(),
                payload_bytes: metadata.size,
                item_count: metadata.item_count,
                path_type,
                peer: Some(TransferPeer {
                    endpoint_id: peer_id.clone(),
                    display_name: Some(peer_name.clone()),
                    device_type: None,
                }),
                save_path: None,
            },
        ));
        let emitter = Arc::new(TransferEmitter {
            weak: weak.clone(),
            recorder: recorder.clone(),
        });
        let app_handle: AppHandle = Some(emitter as Arc<dyn EventEmitter>);
        let (relay_mode, discovery_mode) = match resolve_network(&sync_bg.settings).await {
            Ok(v) => v,
            Err(e) => {
                recorder.finalize(
                    TransferStatus::Failed,
                    None,
                    None,
                    None,
                    Some(e.clone()),
                );
                let msg = format!("Could not configure network: {e}");
                let weak = weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        let state = ui.global::<State>();
                        state.set_share_starting(false);
                        state.set_send_status("".into());
                        state.set_send_error(msg.clone().into());
                        drop(state);
                        toast(&ui, &msg, true);
                    }
                });
                return;
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
            std::time::Duration::from_secs(60),
            start_share_items(paths, options, &app_handle, Some(metadata)),
        )
        .await
        {
            Ok(Ok(share)) => share,
            _ => {
                recorder.finalize(
                    TransferStatus::Failed,
                    None,
                    None,
                    None,
                    Some("share setup timed out".to_string()),
                );
                let msg = "Could not start share (timed out)".to_string();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        let state = ui.global::<State>();
                        state.set_share_starting(false);
                        state.set_send_status("".into());
                        state.set_send_error(msg.clone().into());
                        drop(state);
                        toast(&ui, &msg, true);
                    }
                });
                return;
            }
        };
        let ticket = share.ticket.clone();
        let handle = ShareHandle {
            send_result: share,
            recorder: Some(recorder),
        };
        {
            let mut slot = sync_bg.share.lock().await;
            if slot.is_some() {
                drop(slot);
                tokio::time::timeout(std::time::Duration::from_secs(2), handle.stop())
                    .await
                    .ok();
                let msg = "Already sending elsewhere".to_string();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        let state = ui.global::<State>();
                        state.set_share_starting(false);
                        state.set_send_status("".into());
                        state.set_send_error(msg.clone().into());
                    }
                });
                return;
            }
            *slot = Some(handle);
        }

        let delivered = match node
            .invite_paired_device(&peer_id, &ticket, item_count, byte_count)
            .await
        {
            Ok(true) => true,
            Ok(false) | Err(_) => false,
        };
        let peer_name = peer_name.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() {
                let state = ui.global::<State>();
                state.set_share_starting(false);
                if delivered {
                    state.set_send_status("Delivered — the peer is pulling your files…".into());
                } else {
                    state.set_send_status("".into());
                    state.set_send_error(
                        format!("Could not reach {peer_name} to deliver the files.").into(),
                    );
                }
            }
        });
    });
}

// -------------------------------------------------- auto-accept receives

fn parse_invite_payload(v: &serde_json::Value) -> (String, u32, u64, String, String) {
    (
        v.get("blob_ticket").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        v.get("file_count").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
        v.get("total_size").and_then(|x| x.as_u64()).unwrap_or(0),
        v.get("sender_name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        v.get("remote_endpoint_id").and_then(|x| x.as_str()).unwrap_or("").to_lowercase(),
    )
}

fn auto_accept_invite(sync: &Sync, payload: serde_json::Value) {
    let (ticket, file_count, total_size, sender_name, id) = parse_invite_payload(&payload);
    if id.is_empty() || ticket.is_empty() {
        return;
    }
    let Some(node) = sync.node.lock().unwrap().clone() else {
        return;
    };

    let sync_bg = sync.clone();
    sync.rt.spawn(async move {
        // Peer display name: their stored name, else the invite's claim.
                let peer_name = node
                    .list_paired()
                    .ok()
                    .and_then(|list: Vec<PairedDeviceInfo>| {
                        list.into_iter()
                            .find(|d| d.endpoint_id.to_lowercase() == id)
                            .map(|d| d.display_name)
                    })
                    .filter(|n| !n.trim().is_empty() && n.trim().to_lowercase() != id)
                    .unwrap_or_else(|| {
                if sender_name.trim().is_empty() || sender_name.trim().to_lowercase() == id {
                    short_id(&id)
                } else {
                    sender_name.trim().to_string()
                }
            });
        let save_dir = {
            let guard = sync_bg.settings.lock().unwrap();
            guard
                .downloads_path()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(sanitize_folder_name(&peer_name, &short_id(&id)))
        };

        if let Err(e) = node.respond_paired_invite(&id, true).await {
            tracing::warn!("failed to accept invite from {id}: {e}");
            return;
        }

        let (relay_mode, discovery_mode) = match resolve_network(&sync_bg.settings).await {
            Ok(v) => v,
            Err(e) => {
                let msg = format!("Could not configure network: {e}");
                let weak = sync_bg.weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        toast(&ui, &msg, true);
                    }
                });
                return;
            }
        };
        let options = ReceiveOptions {
            output_dir: Some(save_dir.clone()),
            relay_mode,
            discovery_mode,
            magic_ipv4_addr: None,
            magic_ipv6_addr: None,
        };
        let recorder = Arc::new(Recorder::new(
            sync_bg.history.clone(),
            TransferDirection::Receive,
            Ctx {
                payload_bytes: total_size,
                item_count: file_count,
                save_path: Some(save_dir.to_string_lossy().into_owned()),
                peer: Some(TransferPeer {
                    endpoint_id: id.clone(),
                    display_name: Some(peer_name.clone()),
                    device_type: None,
                }),
                ..Ctx::default()
            },
        ));
        let emitter = Arc::new(TransferEmitter {
            weak: sync_bg.weak.clone(),
            recorder: recorder.clone(),
        });
        let app_handle: AppHandle = Some(emitter as Arc<dyn EventEmitter>);
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
        sync_bg.recv_cancels.lock().insert(id.clone(), cancel_tx);

        {
            let weak = sync_bg.weak.clone();
            let id2 = id.clone();
            let peer_name = peer_name.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    let state = ui.global::<State>();
                    state.set_receive_active_id(id2.into());
                    state.set_receive_status(
                        format!("Receiving {} item(s) from {peer_name}", file_count).into(),
                    );
                    state.set_receive_progress(0.0);
                    state.set_receive_progress_label("".into());
                    state.set_receive_error("".into());
                    drop(state);
                    toast(&ui, &format!("{peer_name} sent you files — downloading"), false);
                }
            });
        }

        let weak2 = sync_bg.weak.clone();
        let id_done = id.clone();
        let peer_done = peer_name.clone();
        match engine::download(ticket, options, app_handle, cancel_rx).await {
            Ok(_) => {
                sync_bg.recv_cancels.lock().remove(&id_done);
                let sync_done = sync_bg.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    clear_receive_ui_for(&sync_done, &id_done);
                    if let Some(ui) = weak2.upgrade() {
                        toast(&ui, &format!("Transfer from {peer_done} saved"), false);
                        refresh_history(&sync_done);
                    }
                });
            }
            Err(e) if e.to_string() == "cancelled" => {
                recorder.finalize(TransferStatus::Cancelled, None, None, None, None);
                sync_bg.recv_cancels.lock().remove(&id_done);
                let sync_done = sync_bg.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    clear_receive_ui_for(&sync_done, &id_done);
                    if let Some(ui) = weak2.upgrade() {
                        toast(&ui, "Receive cancelled", false);
                    }
                });
            }
            Err(e) => {
                recorder.finalize(TransferStatus::Failed, None, None, None, Some(e.to_string()));
                sync_bg.recv_cancels.lock().remove(&id_done);
                let msg = format!("Receive from {peer_done} failed: {e}");
                let sync_done = sync_bg.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    clear_receive_ui_for(&sync_done, &id_done);
                    if let Some(ui) = weak2.upgrade() {
                        toast(&ui, &msg, true);
                        refresh_history(&sync_done);
                    }
                });
            }
        }
    });
}

// --------------------------------------------------- node event routing

fn handle_main_event(sync: &Sync, name: &str, payload: Option<&str>) {
    let parsed = payload
        .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok());

    match name {
        "device-node-network-ready" => {
            if let Some(ui) = sync.weak.upgrade() {
                ui.global::<State>().set_node_ready(true);
                toast(&ui, "Network ready", false);
            }
        }
        "device-node-network-warming" => {
            if let Some(ui) = sync.weak.upgrade() {
                ui.global::<State>().set_node_ready(false);
                toast(&ui, "Connecting to network…", false);
            }
        }
        "relay-fell-back" => {
            if let Some(ui) = sync.weak.upgrade() {
                let reason = parsed
                    .as_ref()
                    .and_then(|v| v.get("reason"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unreachable relay");
                toast(&ui, &format!("Relay fell back to public: {reason}"), true);
            }
        }
        "device-paired" => {
            let who = parsed
                .as_ref()
                .and_then(|v| v.get("display_name"))
                .and_then(|v| v.as_str())
                .unwrap_or("a peer");
            if let Some(ui) = sync.weak.upgrade() {
                toast(&ui, &format!("Paired with {who}"), false);
            }
            refresh_peers(sync);
            refresh_suggestions(sync);
        }
        "device-unpaired" => {
            refresh_peers(sync);
            refresh_suggestions(sync);
        }
        "paired-device-presence" => refresh_peers(sync),
        "nearby-device-found" | "nearby-device-identified" | "nearby-device-lost" => {
            refresh_suggestions(sync)
        }
        "nearby-unavailable" => {
            let reason = parsed
                .as_ref()
                .and_then(|v| v.get("reason"))
                .and_then(|v| v.as_str())
                .unwrap_or("unavailable")
                .to_string();
            if let Some(ui) = sync.weak.upgrade() {
                ui.global::<State>().set_nearby_note(reason.into());
            }
        }
        "nearby-pair-request-received" => {
            let id = parsed
                .as_ref()
                .and_then(|v| v.get("remote_endpoint_id"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_lowercase();
            let sender = parsed
                .as_ref()
                .and_then(|v| v.get("sender_name"))
                .and_then(|v| v.as_str())
                .unwrap_or("A device")
                .to_string();
            if !id.is_empty() {
                {
                    let mut reqs = sync.pair_requests.lock().unwrap();
                    if !reqs.iter().any(|(i, _)| i == &id) {
                        reqs.push((id, sender.clone()));
                    }
                }
                if let Some(ui) = sync.weak.upgrade() {
                    toast(&ui, &format!("{sender} wants to pair — see “Add a peer”"), false);
                }
                refresh_suggestions(sync);
            }
        }
        "paired-invite-response" => {
            let response = parsed
                .as_ref()
                .and_then(|v| v.get("response"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            // An accept is announced by the `device-paired` event (and its
            // toast); only a decline needs surfacing here.
            if response == "declined" {
                let who = parsed
                    .as_ref()
                    .and_then(|v| v.get("display_name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("that device");
                if let Some(ui) = sync.weak.upgrade() {
                    toast(&ui, &format!("{who} declined your pair request"), true);
                }
            }
        }
        "paired-invite-received" => {
            auto_accept_invite(sync, parsed.unwrap_or_default());
        }
        _ => {}
    }
}

// ------------------------------------------------------------- node start

fn start_node(sync: &Sync) {
    let data_dir = sync
        .settings_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let settings = sync.settings.clone();
    let queue = sync.queue.clone();
    let sync_bg = sync.clone();

    sync.rt.spawn(async move {
        let cfg = settings.lock().unwrap().clone();
        let discovery_mode = cfg.discovery_mode();
        let discoverability = cfg.discoverability();
        let (relay, fell_back) = match resolve_relay_mode_with_fallback(Some(cfg.relay_config_arg()))
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("failed to resolve relay mode at startup: {e}");
                (engine::RelayModeOption::Default, false)
            }
        };
        if fell_back {
            tracing::warn!("custom relay unreachable at startup; fell back to public relays");
        }
        let relay_mode: iroh::endpoint::RelayMode = relay.into();
        let emitter: AppHandle = Some(Arc::new(MainEmitter::new(queue)));
        match NodeService::start(
            &data_dir,
            relay_mode,
            discovery_mode,
            discoverability,
            emitter,
        )
        .await
        {
            Ok(node) => {
                *sync_bg.node.lock().unwrap() = Some(Arc::new(node));
                refresh_peers(&sync_bg);
                refresh_suggestions(&sync_bg);
                let weak = sync_bg.weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        toast(&ui, "Connected — peering is live", false);
                    }
                });
            }
            Err(e) => {
                tracing::error!("failed to start node service: {e}");
                let msg = format!("Node start failed: {e}");
                let weak = sync_bg.weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        toast(&ui, &msg, true);
                    }
                });
            }
        }
    });
}

// -------------------------------------------------------- register_* fns

fn register_node(sync: &Sync) {
    start_node(sync);
    let timer = slint::Timer::default();
    let queue = sync.queue.clone();
    let sync_bg = sync.clone();
    timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(150), move || {
        for (name, payload) in queue.drain() {
            let sync = sync_bg.clone();
            let _ = slint::invoke_from_event_loop(move || {
                handle_main_event(&sync, &name, payload.as_deref());
            });
        }
    });
    std::mem::forget(timer);
}

fn begin_rename(sync: Sync) {
    if let Some(ui) = sync.weak.upgrade() {
        let state = ui.global::<State>();
        state.set_rename_mode(true);
        state.set_rename_input(state.get_selected_name());
    }
}

fn confirm_rename(sync: Sync) {
    let Some(node) = sync.node.lock().unwrap().clone() else {
        return;
    };
    let (id, name) = {
        let Some(ui) = sync.weak.upgrade() else {
            return;
        };
        let state = ui.global::<State>();
        (state.get_selected_id().to_string(), state.get_rename_input().to_string().trim().to_string())
    };
    if id.is_empty() || name.is_empty() {
        return;
    }
    let weak = sync.weak.clone();
    let sync_bg = sync.clone();
    sync.rt.spawn(async move {
        match node.rename_paired(&id, &name) {
            Ok(_) => {
                let sync_done = sync_bg.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.global::<State>().set_rename_mode(false);
                        toast(&ui, "Peer renamed", false);
                        refresh_peers(&sync_done);
                    }
                });
            }
            Err(e) => {
                let msg = format!("Rename failed: {e}");
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        toast(&ui, &msg, true);
                    }
                });
            }
        }
    });
}

fn register_peers(sync: &Sync) {
    let Some(ui) = sync.weak.upgrade() else {
        return;
    };
    let logic = ui.global::<Logic>();

    {
        let sync = sync.clone();
        logic.on_refresh_peers(move || {
            refresh_peers(&sync);
            refresh_suggestions(&sync);
        });
    }
    {
        let sync = sync.clone();
        logic.on_select_peer(move |id: SharedString| {
            let id = id.to_string().to_lowercase();
            if let Some((name, detail, online)) = sync.peer_meta.lock().unwrap().get(&id).cloned() {
                if let Some(ui) = sync.weak.upgrade() {
                    let state = ui.global::<State>();
                    state.set_selected_id(id.into());
                    state.set_selected_name(name.into());
                    state.set_selected_detail(detail.into());
                    state.set_selected_online(online);
                    drop(state);
                }
                refresh_history(&sync);
            }
        });
    }
    {
        let sync7 = sync.clone();
        logic.on_rename_peer(move || {
            begin_rename(sync7.clone());
        });
    }
    {
        let sync = sync.clone();
        logic.on_rename_peer_cancel(move || {
            if let Some(ui) = sync.weak.upgrade() {
                ui.global::<State>().set_rename_mode(false);
            }
        });
    }
    {
        let sync5 = sync.clone();
        logic.on_rename_peer_confirm(move || {
            confirm_rename(sync5.clone());
        });
    }
    {
        let sync = sync.clone();
        logic.on_remove_peer(move || {
            let Some(node) = sync.node.lock().unwrap().clone() else {
                return;
            };
            let (id, name) = {
                let Some(ui) = sync.weak.upgrade() else { return };
                let state = ui.global::<State>();
                let id = state.get_selected_id().to_string();
                let name = state.get_selected_name().to_string();
                drop(state);
                (id, name)
            };
            if id.is_empty() {
                return;
            }
            let weak = sync.weak.clone();
            let sync_bg = sync.clone();
            sync.rt.spawn(async move {
                if let Err(e) = node.forget_paired(&id).await {
                    tracing::warn!("forget {id} failed: {e}");
                }
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        let state = ui.global::<State>();
                        state.set_selected_id("".into());
                        state.set_selected_name("".into());
                        state.set_selected_detail("".into());
                        state.set_selected_online(false);
                        state.set_history(ModelRc::from(Rc::new(
                            VecModel::from(Vec::<HistoryRow>::new()),
                        )));
                        drop(state);
                        toast(&ui, &format!("Forgot {name}"), false);
                        refresh_peers(&sync_bg);
                    }
                });
            });
        });
    }
    {
        // Send flow: pick files, share them and deliver to the selected peer.
        let sync = sync.clone();
        logic.on_send_to_peer(move || {
            let Some(node) = sync.node.lock().unwrap().clone() else {
                return;
            };
            let (peer_id, peer_name) = {
                let Some(ui) = sync.weak.upgrade() else { return };
                let state = ui.global::<State>();
                let p = state.get_selected_id().to_string();
                let n = state.get_selected_name().to_string();
                drop(ui);
                if p.is_empty() {
                    return;
                }
                (p, n)
            };
            let sync_bg = sync.clone();
            let touch = {
                let Some(ui) = sync.weak.upgrade() else { return };
                ui.global::<State>().get_touch()
            };
            let rt_here = sync.rt.clone();
            let weak_bg = sync.weak.clone();
            rt_here.spawn_blocking(move || {
                let picked = pick_send_paths();
                if picked.is_empty() {
                    // Touch devices pick content through the system share
                    // sheet; say so instead of silently doing nothing.
                    if touch {
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak_bg.upgrade() {
                                toast(
                                    &ui,
                                    "Use the system share sheet (\"Send to TunnelManager\") to stage files, then pick a peer here.",
                                    false,
                                );
                            }
                        });
                    }
                    return;
                }
                *sync_bg.outbox.lock().unwrap() = picked.clone();
                let sync_bg = sync_bg.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    start_send(sync_bg, node, peer_id, peer_name, picked);
                });
            });
        });
    }
    {
        // stop_share
        let sync = sync.clone();
        logic.on_stop_share(move || {
            let weak = sync.weak.clone();
            let sync_bg = sync.clone();
            sync.rt.spawn(async move {
                let handle = sync_bg.share.lock().await.take();
                if let Some(handle) = handle {
                    if let Some(recorder) = &handle.recorder {
                        recorder.finalize(TransferStatus::Cancelled, None, None, None, None);
                    }
                    handle.stop().await;
                }
                // Android: staged share-sheet files are only needed while
                // the share is open; peers may still pull until stop.
                let staged = sync_bg.outbox.lock().unwrap().drain(..).collect::<Vec<_>>();
                if !staged.is_empty() {
                    android::clear_outbox(&staged);
                }
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        let state = ui.global::<State>();
                        state.set_share_starting(false);
                        state.set_send_status("".into());
                        state.set_send_error("".into());
                        state.set_send_progress(0.0);
                        state.set_send_progress_label("".into());
                        state.set_send_peers(0);
                        state.set_send_active_id("".into());
                        state.set_outbox_count(0);
                        drop(state);
                        toast(&ui, "Send stopped", false);
                        refresh_history(&sync_bg);
                    }
                });
            });
        });
    }
    {
        // cancel an auto-accepted receive on the selected peer
        let sync = sync.clone();
        logic.on_cancel_receive(move || {
            let id = {
                let Some(ui) = sync.weak.upgrade() else { return };
                let s = ui.global::<State>();
                s.get_selected_id().to_string().to_lowercase()
            };
            if let Some(tx) = sync.recv_cancels.lock().remove(&id) {
                let _ = tx.send(());
            }
        });
    }
}

fn register_add_peer(sync: &Sync) {
    let Some(ui) = sync.weak.upgrade() else {
        return;
    };
    let logic = ui.global::<Logic>();

    {
        // Paste an address → join_pairing
        let sync = sync.clone();
        logic.on_pair_with_pasted(move || {
            let Some(node) = sync.node.lock().unwrap().clone() else {
                return;
            };
            let (ticket, weak, sync_bg) = {
                let Some(ui) = sync.weak.upgrade() else { return };
                let state = ui.global::<State>();
                let ticket = state.get_add_ticket_input().to_string().trim().to_string();
                drop(ui);

                if ticket.is_empty() {
                    let weak = sync.weak.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak.upgrade() {
                            let s = ui.global::<State>();
                            s.set_pairing_status("Paste an address first".into());
                            s.set_pairing_error(true);
                        }
                    });
                    return;
                }

                if let Some(ui) = sync.weak.upgrade() {
                    let s = ui.global::<State>();
                    s.set_pairing_busy(true);
                    s.set_pairing_status("Connecting…".into());
                    s.set_pairing_error(false);
                }
                (ticket, sync.weak.clone(), sync.clone())
            };

            let rt_bg = sync_bg.rt.clone();
            rt_bg.spawn(async move {
                match node.join_pairing(&ticket).await {
                    Ok(()) => {
                        let weak2 = weak.clone();
                        let sync_bg2 = sync_bg.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak2.upgrade() {
                                let s = ui.global::<State>();
                                s.set_pairing_busy(false);
                                s.set_pairing_status("Peer added".into());
                                s.set_pairing_error(false);
                                s.set_add_ticket_input("".into());
                            }
                        });
                        let sync_bg3 = sync_bg.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            refresh_peers(&sync_bg2);
                            refresh_suggestions(&sync_bg3);
                        });
                    }
                    Err(e) => {
                        let msg = format!("Could not pair: {e}");
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak.upgrade() {
                                let s = ui.global::<State>();
                                s.set_pairing_busy(false);
                                s.set_pairing_status(msg.clone().into());
                                s.set_pairing_error(true);
                            }
                        });
                    }
                }
            });
        });
    }

    {
        // Pair a suggestion: an inbound pair request is accepted with the
        // already-committed invite; a nearby device gets a pair request the
        // other side must still accept, so it reports "sent", not "added".
        let sync = sync.clone();
        logic.on_accept_suggestion(move |id: SharedString| {
            let Some(node) = sync.node.lock().unwrap().clone() else {
                return;
            };
            let weak = sync.weak.clone();
            let sync_bg = sync.clone();
            sync.rt.spawn(async move {
                let id = id.to_string();
                let is_request = sync_bg
                    .pair_requests
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(i, _)| i == &id.to_lowercase());

                let (success, status) = if is_request {
                    match node.accept_nearby_invite(&id).await {
                        Ok(()) => {
                            sync_bg
                                .pair_requests
                                .lock()
                                .unwrap()
                                .retain(|(i, _)| i != &id.to_lowercase());
                            (true, "Peer added".to_string())
                        }
                        Err(e) => (false, format!("Could not pair: {e}")),
                    }
                } else {
                    match node.request_nearby_pair(&id).await {
                        Ok(true) => (
                            true,
                            "Pair request sent — waiting for them to accept".to_string(),
                        ),
                        Ok(false) => (
                            false,
                            "Couldn't reach them — are you on the same network?".to_string(),
                        ),
                        Err(e) => (false, format!("Could not pair: {e}")),
                    }
                };

                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        let s = ui.global::<State>();
                        s.set_pairing_status(status.into());
                        s.set_pairing_error(!success);
                    }
                });
                refresh_peers(&sync_bg);
                refresh_suggestions(&sync_bg);
            });
        });
    }

    {
        // decline an inbound pair request
        let sync = sync.clone();
        logic.on_decline_suggestion(move |id: SharedString| {
            let Some(node) = sync.node.lock().unwrap().clone() else {
                return;
            };
            {
                let mut reqs = sync.pair_requests.lock().unwrap();
                reqs.retain(|(i, _)| i != &id.to_string().to_lowercase());
            }
            let sync_bg = sync.clone();
            sync.rt.spawn(async move {
                if let Err(e) = node.decline_nearby_invite(&id, false).await {
                    tracing::warn!("decline {id} failed: {e}");
                }
                refresh_suggestions(&sync_bg);
            });
        });
    }

    {
        // copy my pairing ticket
        let weak = sync.weak.clone();
        logic.on_copy_my_ticket(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let ticket = ui.global::<State>().get_my_ticket().to_string();
            if ticket.is_empty() {
                return;
            }
            match copy_to_clipboard(&ticket) {
                Ok(()) => toast(&ui, "Invite copied — share it with your peer", false),
                Err(e) => toast(&ui, &format!("Copy failed: {e}"), true),
            }
        });
    }
}

fn register_history(sync: &Sync) {
    let Some(ui) = sync.weak.upgrade() else {
        return;
    };
    let logic = ui.global::<Logic>();

    {
        let sync = sync.clone();
        logic.on_refresh_history(move || refresh_history(&sync));
    }
    {
        // delete an entry + reclaim its partial store
        let sync = sync.clone();
        logic.on_delete_row(move |id: SharedString| {
            let sync_bg = sync.clone();
            sync.rt.spawn_blocking(move || {
                match sync_bg.history.delete(id.as_ref()) {
                    Ok(Some(record)) => {
                        let temp_dir = engine::storage::temp_dir();
                        if let Some(raw) = record.resumable_store_path.as_deref() {
                            let path = PathBuf::from(raw);
                            if is_reclaimable_partial(&path, &temp_dir) {
                                reclaim_partial(&record, &temp_dir);
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!("failed to delete history row: {e}"),
                }
                let _ = slint::invoke_from_event_loop(move || {
                    refresh_history(&sync_bg);
                });
            });
        });
    }
    {
        let sync = sync.clone();
        logic.on_open_row(move |id: SharedString| {
            let sync_bg = sync.clone();
            sync.rt.spawn_blocking(move || {
                let id = id.to_string();
                let target = sync_bg
                    .history
                    .list()
                    .ok()
                    .and_then(|records| {
                        records
                            .iter()
                            .find(|r| r.id == id)
                            .and_then(|r| r.save_path.clone())
                    });
                if let Some(target) = target {
                    open_path(&target);
                }
            });
        });
    }
}

fn register_settings(sync: &Sync) {
    let Some(ui) = sync.weak.upgrade() else {
        return;
    };
    let logic = ui.global::<Logic>();

    {
        let sync = sync.clone();
        logic.on_pick_downloads_dir(move || {
            let sync_bg = sync.clone();
            let rt_here = sync_bg.rt.clone();
            rt_here.spawn_blocking(move || {
                if let Some(folder) = pick_downloads_folder() {
                    let text = folder.to_string_lossy().into_owned();
                    let weak = sync_bg.weak.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak.upgrade() {
                            ui.global::<State>().set_downloads_dir(text.into());
                        }
                    });
                }
            });
        });
    }

    {
        // Name ourselves
        let sync = sync.clone();
        logic.on_rename_self_name(move || {
            let Some(node) = sync.node.lock().unwrap().clone() else {
                return;
            };
            let name = {
                let Some(ui) = sync.weak.upgrade() else { return };
                let s = ui.global::<State>();
                s.get_my_name().to_string().trim().to_string()
            };
            if name.is_empty() {
                return;
            }
            let weak = sync.weak.clone();
            let sync_bg = sync.clone();
            sync.rt.spawn(async move {
                match node.set_device_display_name(&name) {
                    Ok(info) => {
                        let weak2 = weak.clone();
                        let sync_bg2 = sync_bg.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak2.upgrade() {
                                ui.global::<State>()
                                    .set_my_name(info.display_name.clone().into());
                                toast(&ui, "Device name saved", false);
                                let _ = &info;
                            }
                        });
                        refresh_peers(&sync_bg2);
                    }
                    Err(e) => {
                        let msg = format!("Could not set name: {e}");
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak.upgrade() {
                                toast(&ui, &msg, true);
                            }
                        });
                    }
                }
            });
        });
    }

    {
        let sync = sync.clone();
        logic.on_save_settings(move || {
            let new_settings = {
                let Some(ui) = sync.weak.upgrade() else { return };
                let state = ui.global::<State>();
                let s = Settings {
                    downloads_dir: {
                        let dir = state.get_downloads_dir().to_string();
                        (!dir.trim().is_empty()).then_some(dir)
                    },
                    relay_mode: match state.get_relay_mode() {
                        1 => "disabled".to_string(),
                        2 => "custom".to_string(),
                        _ => "default".to_string(),
                    },
                    relay_urls: state
                        .get_relay_urls()
                        .to_string()
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .map(str::to_string)
                        .collect(),
                    relay_token: {
                        let token = state.get_relay_token().to_string();
                        (!token.trim().is_empty()).then_some(token)
                    },
                    relay_fallback: match state.get_relay_fallback() {
                        1 => "public".to_string(),
                        _ => "strict".to_string(),
                    },
                    discovery_mode: match state.get_discovery_mode() {
                        1 => "custom".to_string(),
                        _ => "default".to_string(),
                    },
                    discovery_pkarr_relay_url: {
                        let url = state.get_discovery_pkarr_url().to_string();
                        (!url.trim().is_empty()).then_some(url)
                    },
                    discovery_dns_origin: {
                        let origin = state.get_discovery_dns_origin().to_string();
                        (!origin.trim().is_empty()).then_some(origin)
                    },
                    history_enabled: state.get_history_enabled(),
                    discoverability: match state.get_discoverability() {
                        1 => "paired-only".to_string(),
                        2 => "off".to_string(),
                        _ => "everyone".to_string(),
                    },
                };
                drop(state);
                drop(ui);
                s
            };

            *sync.settings.lock().unwrap() = new_settings.clone();
            let path = sync.settings_path.clone();
            let weak = sync.weak.clone();
            let sync_bg = sync.clone();
            sync.rt.spawn_blocking(move || match new_settings.save(&path) {
                Ok(()) => {
                    let weak = weak.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak.upgrade() {
                            ui.global::<State>().set_settings_status("Saved.".into());
                            toast(&ui, "Settings saved", false);
                        }
                    });
                    let Some(node) = sync_bg.node.lock().unwrap().clone() else {
                        return;
                    };
                    let (relay, discovery, disc) = {
                        let cfg = sync_bg.settings.lock().unwrap().clone();
                        (cfg.relay_mode(), cfg.discovery_mode(), cfg.discoverability())
                    };
                    sync_bg.rt.spawn(async move {
                        if let Err(e) = node.reconfigure_network(relay.into(), discovery).await {
                            tracing::warn!("reconfigure failed: {e}");
                        }
                        if let Err(e) = node.set_discoverability(disc).await {
                            tracing::warn!("discoverability change failed: {e}");
                        }
                    });
                }
                Err(e) => {
                    let msg = format!("Could not save settings: {e}");
                    let weak = weak.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak.upgrade() {
                            ui.global::<State>().set_settings_status(msg.clone().into());
                            toast(&ui, &msg, true);
                        }
                    });
                }
            });
        });
    }

    {
        let sync = sync.clone();
        logic.on_test_relay(move || {
            let Some(ui) = sync.weak.upgrade() else {
                return;
            };
            let arg = relay_arg_from_state(&ui);
            let weak = sync.weak.clone();
            sync.rt.spawn(async move {
                let _ = slint::invoke_from_event_loop({
                    let weak = weak.clone();
                    move || {
                        if let Some(ui) = weak.upgrade() {
                            let s = ui.global::<State>();
                            s.set_relay_testing(true);
                            s.set_relay_test_status("Testing…".into());
                        }
                    }
                });
                let result = verify_relays(arg).await;
                let _ = slint::invoke_from_event_loop({
                    let weak = weak.clone();
                    move || {
                        if let Some(ui) = weak.upgrade() {
                            let s = ui.global::<State>();
                            s.set_relay_testing(false);
                            match result {
                                Ok(resp) => {
                                    let msg = match resp.url {
                                        Some(url) => {
                                            format!("Connected to {url} ({}ms)", resp.latency_ms)
                                        }
                                        None => format!("Connected ({}ms)", resp.latency_ms),
                                    };
                                    s.set_relay_test_status(msg.into());
                                    toast(&ui, "Relay connection verified", false);
                                }
                                Err(e) => {
                                    let msg = format!("Relay check failed: {e}");
                                    s.set_relay_test_status(msg.clone().into());
                                    toast(&ui, &msg, true);
                                }
                            }
                        }
                    }
                });
            });
        });
    }

    {
        let sync = sync.clone();
        logic.on_check_relay_status(move || {
            let Some(ui) = sync.weak.upgrade() else {
                return;
            };
            let arg = relay_arg_from_state(&ui);
            let weak = sync.weak.clone();
            sync.rt.spawn(async move {
                match get_relay_status(Some(arg)).await {
                    Ok(resp) => {
                        let label = match resp.kind.as_str() {
                            "disabled" => "Relay disabled".to_string(),
                            "custom" => format!(
                                "Custom relay: {}",
                                resp.url.as_deref().unwrap_or("unreachable")
                            ),
                            "public" => format!(
                                "Public relay: {}",
                                resp.url.as_deref().unwrap_or("n0")
                            ),
                            _ => "Relay unavailable".to_string(),
                        };
                        let fell_back = resp.fell_back_to_public;
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak.upgrade() {
                                ui.global::<State>().set_relay_status(label.into());
                                if fell_back {
                                    toast(
                                        &ui,
                                        "Custom relay unreachable — using public relays",
                                        true,
                                    );
                                }
                            }
                        });
                    }
                    Err(e) => {
                        let msg = format!("Relay status failed: {e}");
                        let _ = slint::invoke_from_event_loop({
                            let weak = weak.clone();
                            move || {
                                if let Some(ui) = weak.upgrade() {
                                    ui.global::<State>().set_relay_status(msg.clone().into());
                                    toast(&ui, &msg, true);
                                }
                            }
                        });
                    }
                }
            });
        });
    }

    {
        let sync = sync.clone();
        logic.on_test_discovery(move || {
            let Some(ui) = sync.weak.upgrade() else {
                return;
            };
            let arg = discovery_arg_from_state(&ui);
            let weak = sync.weak.clone();
            sync.rt.spawn(async move {
                let _ = slint::invoke_from_event_loop({
                    let weak = weak.clone();
                    move || {
                        if let Some(ui) = weak.upgrade() {
                            let s = ui.global::<State>();
                            s.set_discovery_testing(true);
                            s.set_discovery_test_status("Testing…".into());
                        }
                    }
                });
                let result = verify_discovery(arg).await;
                let _ = slint::invoke_from_event_loop({
                    let weak = weak.clone();
                    move || {
                        if let Some(ui) = weak.upgrade() {
                            let s = ui.global::<State>();
                            s.set_discovery_testing(false);
                            match result {
                                Ok(resp) => {
                                    let msg = match resp.url {
                                        Some(url) => format!(
                                            "Discovery server reachable: {url} ({}ms)",
                                            resp.latency_ms
                                        ),
                                        None => format!("Reachable ({}ms)", resp.latency_ms),
                                    };
                                    s.set_discovery_test_status(msg.into());
                                    toast(&ui, "Discovery server verified", false);
                                }
                                Err(e) => {
                                    let msg = format!("Discovery check failed: {e}");
                                    s.set_discovery_test_status(msg.clone().into());
                                    toast(&ui, &msg, true);
                                }
                            }
                        }
                    }
                });
            });
        });
    }

    {
        let sync = sync.clone();
        logic.on_page_changed(move |page: SharedString| {
            let page = page.to_string();
            if page == "peer" {
                refresh_peers(&sync);
                refresh_suggestions(&sync);
                refresh_history(&sync);
            } else if page == "add-peer" {
                refresh_suggestions(&sync);
                refresh_peers(&sync);
            }
        });
    }
}

/// Shared startup: settings, history store, node service and UI wiring.
/// Called from `main()` on desktop and `android_main()` on Android.
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let data_dir = std::env::var("TUNNELMANAGER_SLINT_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| default_data_dir());
    let _ = std::fs::create_dir_all(&data_dir);

    let settings_path = data_dir.join("settings.json");
    let settings = Arc::new(Mutex::new(Settings::load(&settings_path)));

    let history = Arc::new(TransferHistoryStore::new(&data_dir));
    if let Err(e) = history.mark_interrupted() {
        tracing::warn!("history interrupt sweep failed: {e}");
    }

    let ui = AppWindow::new().expect("failed to create AppWindow");
    let main_queue = MainQueue::default();

    let initial_downloads_dir = settings
        .lock()
        .unwrap()
        .downloads_path()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    {
        let s = settings.lock().unwrap();
        let state = ui.global::<State>();
        state.set_downloads_dir(initial_downloads_dir.clone().into());
        state.set_relay_mode(match s.relay_mode.as_str() {
            "disabled" => 1,
            "custom" => 2,
            _ => 0,
        });
        state.set_relay_urls(s.relay_urls.join("\n").into());
        state.set_relay_token(s.relay_token.clone().unwrap_or_default().into());
        state.set_relay_fallback(match s.relay_fallback.as_str() {
            "public" => 1,
            _ => 0,
        });
        state.set_discovery_mode(match s.discovery_mode.as_str() {
            "custom" => 1,
            _ => 0,
        });
        state.set_discovery_pkarr_url(s.discovery_pkarr_relay_url.clone().unwrap_or_default().into());
        state.set_discovery_dns_origin(s.discovery_dns_origin.clone().unwrap_or_default().into());
        state.set_discoverability(match s.discoverability.as_str() {
            "paired-only" => 1,
            "off" => 2,
            _ => 0,
        });
        state.set_history_enabled(s.history_enabled);
        state.set_my_name("…".into());
    }

    // Responsive layout flags: single-pane + bottom nav on touch devices.
    let state = ui.global::<State>();
    state.set_touch(WINDOW_TOUCH);
    state.set_compact(WINDOW_TOUCH);
    // Android "intent listener": content shared into the app ("Send to
    // TunnelManager") is staged at startup, the outbox hint appears on the
    // peer page and the user is nudged to select a peer to send to. Files
    // shared while only the app was in the background cannot be observed
    // (android-activity drops onNewIntent); that share restarts the activity.
    #[cfg(target_os = "android")]
    let staged = android::pick_send_files();

    let sync = Sync {
        weak: ui.as_weak(),
        rt: rt.handle().clone(),
        share: Arc::new(tokio::sync::Mutex::new(None)),
        history,
        settings,
        settings_path: settings_path.clone(),
        node: Arc::new(Mutex::new(None)),
        peer_meta: Arc::new(Mutex::new(HashMap::new())),
        pair_requests: Arc::new(Mutex::new(Vec::new())),
        recv_cancels: ReceiveCancels::default(),
        queue: main_queue.clone(),
        outbox: Arc::new(Mutex::new(Vec::new())),
    };
    refresh_peers(&sync);

    #[cfg(target_os = "android")]
    if !staged.is_empty() {
        *sync.outbox.lock().unwrap() = staged.clone();
        let count: i32 = staged.len() as i32;
        state.set_outbox_count(count);
        state.set_page("peer".into());
        // Auto-act: opening the compact peer picker lets the user choose a
        // peer for the staged files in one tap.
        state.set_show_peer_picker(true);
        toast(&ui, &format!("{count} file(s) shared — pick a peer to send them"), false);
    }
    #[cfg(not(target_os = "android"))]
    state.set_outbox_count(0);

    register_node(&sync);
    register_peers(&sync);
    register_add_peer(&sync);
    register_history(&sync);
    register_settings(&sync);

    let _ = main_queue;
    let _ = settings_path;

    ui.run().expect("UI error");
}

/// Base directory for settings + history store. On Android this is the
/// app-private data dir (`/data/data/<pkg>/files`); `dirs::data_dir()` has
/// no meaning there.
#[cfg(target_os = "android")]
fn default_data_dir() -> PathBuf {
    android::data_dir()
}

#[cfg(not(target_os = "android"))]
fn default_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tunnelmanager-slint")
}
