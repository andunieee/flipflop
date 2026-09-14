mod emitter;
mod format;
mod recorder;
mod settings;

slint::include_modules!();

use emitter::GuiEmitter;
use recorder::{Ctx, Recorder};
use settings::Settings;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel, Weak};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use engine::{
    download, fetch_metadata, reclaim_partial, start_share_items, AddrInfoOptions, AppHandle,
    DiscoveryModeOption, EventEmitter, FileMetadata, ReceiveOptions, SendOptions,
    TransferDirection, TransferHistoryStore, TransferPathType, TransferRecord, TransferStatus,
};

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

struct App {
    weak: Weak<AppWindow>,
    rt: tokio::runtime::Handle,
    send_paths: Arc<Mutex<Vec<PathBuf>>>,
    share: Arc<tokio::sync::Mutex<Option<ShareHandle>>>,
    recv_cancel: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
    history: Arc<TransferHistoryStore>,
    settings: Arc<Mutex<Settings>>,
    settings_path: PathBuf,
}

fn toast(ui: &AppWindow, msg: &str, error: bool) {
    let state = ui.global::<State>();
    state.set_toast(msg.into());
    state.set_toast_error(error);
}

fn default_downloads_dir() -> Option<PathBuf> {
    dirs::download_dir()
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

fn path_type_of(paths: &[PathBuf]) -> Option<TransferPathType> {
    if paths.len() > 1 {
        None
    } else if paths.first().map(|p| p.is_dir()).unwrap_or(false) {
        Some(TransferPathType::Directory)
    } else {
        Some(TransferPathType::File)
    }
}

fn metadata_for(paths: &[PathBuf]) -> FileMetadata {
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
    FileMetadata {
        file_name,
        item_count: paths.len() as u32,
        size,
        thumbnail: None,
        mime_type,
        items: None,
    }
}

fn send_item_model(paths: &[PathBuf]) -> ModelRc<SendItem> {
    let items: Vec<SendItem> = paths
        .iter()
        .map(|p| SendItem {
            name: path_name(p).into(),
            size: format::fmt_bytes(dir_size(p)).into(),
            is_dir: p.is_dir(),
        })
        .collect();
    ModelRc::from(Rc::new(VecModel::from(items)))
}

fn status_text(status: &TransferStatus) -> &'static str {
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
        status: status_text(&record.status).into(),
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

fn main() {
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

    let data_dir = std::env::var("DASHBEAM_SLINT_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("dashbeam-slint")
        });
    let _ = std::fs::create_dir_all(&data_dir);

    let settings_path = data_dir.join("settings.json");
    let settings = Arc::new(Mutex::new(Settings::load(&settings_path)));

    let history = Arc::new(TransferHistoryStore::new(&data_dir));
    if let Err(e) = history.mark_interrupted() {
        tracing::warn!("history interrupt sweep failed: {e}");
    }

    let ui = AppWindow::new().expect("failed to create AppWindow");

    let downloads_default = settings
        .lock()
        .unwrap()
        .downloads_path()
        .map(|p| p.to_string_lossy().into_owned())
        .or_else(|| default_downloads_dir().map(|p| p.to_string_lossy().into_owned()))
        .unwrap_or_default();

    {
        let s = settings.lock().unwrap();
        let state = ui.global::<State>();
        state.set_downloads_dir(s.downloads_dir.clone().unwrap_or_default().into());
        state.set_relay_mode(match s.relay_mode.as_str() {
            "disabled" => 1,
            "custom" => 2,
            _ => 0,
        });
        state.set_relay_urls(s.relay_urls.join("\n").into());
        state.set_relay_token(s.relay_token.clone().unwrap_or_default().into());
        state.set_history_enabled(s.history_enabled);
        state.set_save_dir(downloads_default.clone().into());
    }

    let app = Rc::new(App {
        weak: ui.as_weak(),
        rt: rt.handle().clone(),
        send_paths: Arc::new(Mutex::new(Vec::new())),
        share: Arc::new(tokio::sync::Mutex::new(None)),
        recv_cancel: Arc::new(Mutex::new(None)),
        history: history.clone(),
        settings: settings.clone(),
        settings_path: settings_path.clone(),
    });

    register_send(&ui, &app);
    register_receive(&ui, &app);
    register_history(&ui, &app);
    register_settings(&ui, &app);

    // Initial history load once the event loop is up.
    {
        let app = app.clone();
        slint::Timer::single_shot(std::time::Duration::from_millis(0), move || {
            refresh_history(&app);
        });
    }

    ui.run().expect("UI error");
}

fn register_send(ui: &AppWindow, app: &Rc<App>) {
    let logic = ui.global::<Logic>();

    {
        let app = app.clone();
        logic.on_pick_files(move || {
            let rt = app.rt.clone();
            let weak = app.weak.clone();
            let send_paths = app.send_paths.clone();
            rt.spawn_blocking(move || {
                let picked = rfd::FileDialog::new()
                    .set_title("Choose files to share")
                    .pick_files();
                if let Some(files) = picked {
                    let _ = slint::invoke_from_event_loop(move || {
                        let mut paths = send_paths.lock().unwrap();
                        for file in files {
                            if !paths.contains(&file) {
                                paths.push(file);
                            }
                        }
                        let snapshot = paths.clone();
                        drop(paths);
                        if let Some(ui) = weak.upgrade() {
                            ui.global::<State>()
                                .set_send_items(send_item_model(&snapshot));
                        }
                    });
                }
            });
        });
    }

    {
        let app = app.clone();
        logic.on_pick_folder(move || {
            let rt = app.rt.clone();
            let weak = app.weak.clone();
            let send_paths = app.send_paths.clone();
            rt.spawn_blocking(move || {
                let picked = rfd::FileDialog::new()
                    .set_title("Choose a folder to share")
                    .pick_folder();
                if let Some(folder) = picked {
                    let _ = slint::invoke_from_event_loop(move || {
                        let mut paths = send_paths.lock().unwrap();
                        if !paths.contains(&folder) {
                            paths.push(folder);
                        }
                        let snapshot = paths.clone();
                        drop(paths);
                        if let Some(ui) = weak.upgrade() {
                            ui.global::<State>()
                                .set_send_items(send_item_model(&snapshot));
                        }
                    });
                }
            });
        });
    }

    {
        let app = app.clone();
        logic.on_remove_item(move |index: i32| {
            let mut paths = app.send_paths.lock().unwrap();
            if index < 0 {
                paths.clear();
            } else {
                let index = index as usize;
                if index < paths.len() {
                    paths.remove(index);
                }
            }
            let snapshot = paths.clone();
            drop(paths);
            if let Some(ui) = app.weak.upgrade() {
                ui.global::<State>()
                    .set_send_items(send_item_model(&snapshot));
            }
        });
    }

    {
        let app = app.clone();
        logic.on_start_share(move || {
            let Some(ui) = app.weak.upgrade() else {
                return;
            };
            let state = ui.global::<State>();
            if state.get_share_active() || state.get_share_starting() {
                return;
            }
            let paths = app.send_paths.lock().unwrap().clone();
            if paths.is_empty() {
                return;
            }
            state.set_share_starting(true);
            state.set_send_error("".into());
            state.set_send_progress(0.0);
            state.set_send_progress_label("".into());
            state.set_send_peers(0);
            state.set_ticket("".into());
            drop(state);

            let rt = app.rt.clone();
            let settings = app.settings.clone();
            let history = app.history.clone();
            let share_slot = app.share.clone();
            let weak = app.weak.clone();
            rt.spawn(async move {
                let relay_mode = settings.lock().unwrap().relay_mode();
                let options = SendOptions {
                    relay_mode,
                    discovery_mode: DiscoveryModeOption::Default,
                    ticket_type: AddrInfoOptions::RelayAndAddresses,
                    magic_ipv4_addr: None,
                    magic_ipv6_addr: None,
                };
                let metadata = metadata_for(&paths);
                let recorder = Arc::new(Recorder::new(
                    history,
                    TransferDirection::Send,
                    Ctx {
                        root_name: metadata.file_name.clone(),
                        payload_bytes: metadata.size,
                        item_count: metadata.item_count,
                        path_type: path_type_of(&paths),
                        save_path: None,
                    },
                ));
                let emitter = Arc::new(GuiEmitter::new(weak.clone(), Some(recorder.clone())));
                let app_handle: AppHandle = Some(emitter.clone() as Arc<dyn EventEmitter>);

                match start_share_items(paths, options, &app_handle, Some(metadata)).await {
                    Ok(result) => {
                        let ticket = result.ticket.clone();
                        let handle = ShareHandle {
                            send_result: result,
                            recorder: Some(recorder),
                        };
                        let mut slot = share_slot.lock().await;
                        if slot.is_some() {
                            drop(slot);
                            handle.stop().await;
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(ui) = weak.upgrade() {
                                    let state = ui.global::<State>();
                                    state.set_share_starting(false);
                                    state.set_send_error("Already sharing".into());
                                }
                            });
                            return;
                        }
                        *slot = Some(handle);
                        drop(slot);
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak.upgrade() {
                                let state = ui.global::<State>();
                                state.set_share_starting(false);
                                state.set_share_active(true);
                                state.set_ticket(ticket.into());
                                state.set_send_status(
                                    "Waiting for a receiver to pull the share…".into(),
                                );
                                toast(&ui, "Share ready", false);
                            }
                        });
                    }
                    Err(e) => {
                        let msg = format!("Share failed: {e}");
                        tracing::error!("{msg}");
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak.upgrade() {
                                let state = ui.global::<State>();
                                state.set_share_starting(false);
                                state.set_share_active(false);
                                state.set_send_error(msg.into());
                            }
                        });
                    }
                }
            });
        });
    }

    {
        let app = app.clone();
        logic.on_stop_share(move || {
            let share_slot = app.share.clone();
            let weak = app.weak.clone();
            app.rt.spawn(async move {
                let handle = share_slot.lock().await.take();
                if let Some(handle) = handle {
                    if let Some(recorder) = &handle.recorder {
                        recorder.finalize(TransferStatus::Cancelled, None, None, None, None);
                    }
                    handle.stop().await;
                }
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        let state = ui.global::<State>();
                        state.set_share_active(false);
                        state.set_ticket("".into());
                        state.set_send_progress(0.0);
                        state.set_send_progress_label("".into());
                        state.set_send_peers(0);
                        state.set_send_status("".into());
                        toast(&ui, "Sharing stopped", false);
                    }
                });
            });
        });
    }

    {
        let weak = app.weak.clone();
        logic.on_copy_ticket(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let ticket = ui.global::<State>().get_ticket().to_string();
            if ticket.is_empty() {
                return;
            }
            match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(&ticket)) {
                Ok(()) => toast(&ui, "Ticket copied to clipboard", false),
                Err(e) => toast(&ui, &format!("Copy failed: {e}"), true),
            }
        });
    }
}

fn register_receive(ui: &AppWindow, app: &Rc<App>) {
    let logic = ui.global::<Logic>();

    {
        let app = app.clone();
        logic.on_fetch_info(move || {
            let Some(ui) = app.weak.upgrade() else {
                return;
            };
            let state = ui.global::<State>();
            let ticket = state.get_ticket_input().to_string();
            if ticket.is_empty() || state.get_fetching_info() || state.get_receive_active() {
                return;
            }
            state.set_fetching_info(true);
            state.set_info_loaded(false);
            state.set_receive_done(false);
            state.set_receive_error("".into());
            state.set_receive_status("".into());
            drop(state);

            let rt = app.rt.clone();
            let settings = app.settings.clone();
            let weak = app.weak.clone();
            rt.spawn(async move {
                let relay_mode = settings.lock().unwrap().relay_mode();
                let options = ReceiveOptions {
                    output_dir: None,
                    relay_mode,
                    discovery_mode: DiscoveryModeOption::Default,
                    magic_ipv4_addr: None,
                    magic_ipv6_addr: None,
                };
                match fetch_metadata(ticket, options).await {
                    Ok(meta) => {
                        let name = meta.file_name.clone();
                        let size = format::fmt_bytes(meta.size);
                        let count = meta.item_count as i32;
                        let default_dir = settings
                            .lock()
                            .unwrap()
                            .downloads_path()
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak.upgrade() {
                                let state = ui.global::<State>();
                                state.set_fetching_info(false);
                                state.set_info_loaded(true);
                                state.set_receive_done(false);
                                state.set_receive_name(name.into());
                                state.set_receive_size(size.into());
                                state.set_receive_count(count);
                                if state.get_save_dir().is_empty() {
                                    state.set_save_dir(default_dir.into());
                                }
                                state.set_receive_progress(0.0);
                                state.set_receive_progress_label("".into());
                                state.set_receive_speed("".into());
                            }
                        });
                    }
                    Err(e) => {
                        let msg = format!("Could not read ticket: {e}");
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak.upgrade() {
                                let state = ui.global::<State>();
                                state.set_fetching_info(false);
                                state.set_receive_error(msg.into());
                            }
                        });
                    }
                }
            });
        });
    }

    {
        let app = app.clone();
        logic.on_pick_save_dir(move || {
            let rt = app.rt.clone();
            let weak = app.weak.clone();
            rt.spawn_blocking(move || {
                let picked = rfd::FileDialog::new()
                    .set_title("Choose where to save received files")
                    .pick_folder();
                if let Some(folder) = picked {
                    let text = folder.to_string_lossy().into_owned();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak.upgrade() {
                            ui.global::<State>().set_save_dir(text.into());
                        }
                    });
                }
            });
        });
    }

    {
        let app = app.clone();
        logic.on_start_download(move || {
            let Some(ui) = app.weak.upgrade() else {
                return;
            };
            let state = ui.global::<State>();
            if state.get_receive_active() || !state.get_info_loaded() {
                return;
            }
            let ticket = state.get_ticket_input().to_string();
            let save_dir = {
                let configured = state.get_save_dir().to_string();
                if configured.trim().is_empty() {
                    default_downloads_dir()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_else(|| ".".to_string())
                } else {
                    configured
                }
            };
            state.set_receive_active(true);
            state.set_receive_done(false);
            state.set_receive_error("".into());
            state.set_receive_progress(0.0);
            state.set_receive_progress_label("".into());
            state.set_receive_speed("".into());
            state.set_receive_status("Connecting…".into());
            drop(state);

            let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
            *app.recv_cancel.lock().unwrap() = Some(cancel_tx);

            let rt = app.rt.clone();
            let settings = app.settings.clone();
            let history = app.history.clone();
            let weak = app.weak.clone();
            rt.spawn(async move {
                let relay_mode = settings.lock().unwrap().relay_mode();
                let options = ReceiveOptions {
                    output_dir: Some(PathBuf::from(&save_dir)),
                    relay_mode,
                    discovery_mode: DiscoveryModeOption::Default,
                    magic_ipv4_addr: None,
                    magic_ipv6_addr: None,
                };
                let recorder = Arc::new(Recorder::new(
                    history,
                    TransferDirection::Receive,
                    Ctx {
                        save_path: Some(save_dir.clone()),
                        ..Default::default()
                    },
                ));
                let emitter = Arc::new(GuiEmitter::new(weak.clone(), Some(recorder.clone())));
                let app_handle: AppHandle = Some(emitter.clone() as Arc<dyn EventEmitter>);

                match download(ticket, options, app_handle, cancel_rx).await {
                    Ok(_) => {
                        // `receive-completed` already closed the history row
                        // and marked the receive done in the UI.
                    }
                    Err(e) if e.to_string() == "cancelled" => {
                        recorder.finalize(TransferStatus::Cancelled, None, None, None, None);
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak.upgrade() {
                                let state = ui.global::<State>();
                                state.set_receive_active(false);
                                state.set_receive_status(
                                    "Cancelled — partial download kept for resume".into(),
                                );
                                state.set_receive_speed("".into());
                                toast(&ui, "Download cancelled", false);
                            }
                        });
                    }
                    Err(e) => {
                        recorder.finalize(
                            TransferStatus::Failed,
                            None,
                            None,
                            None,
                            Some(e.to_string()),
                        );
                        let msg = format!("Download failed: {e}");
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak.upgrade() {
                                let state = ui.global::<State>();
                                state.set_receive_active(false);
                                state.set_receive_speed("".into());
                                state.set_receive_error(msg.into());
                            }
                        });
                    }
                }
            });
        });
    }

    {
        let app = app.clone();
        logic.on_cancel_download(move || {
            let sender = app.recv_cancel.lock().unwrap().take();
            if let Some(sender) = sender {
                let _ = sender.send(());
            }
        });
    }

    {
        let weak = app.weak.clone();
        logic.on_open_received(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let dir = ui.global::<State>().get_received_dir().to_string();
            if dir.is_empty() {
                return;
            }
            let dir = PathBuf::from(dir);
            std::thread::spawn(move || {
                if let Err(e) = open::that(&dir) {
                    tracing::warn!("failed to open {dir:?}: {e}");
                }
            });
        });
    }
}

fn refresh_history(app: &Rc<App>) {
    let rt = app.rt.clone();
    let history = app.history.clone();
    let weak = app.weak.clone();
    rt.spawn_blocking(move || {
        let rows: Vec<HistoryRow> = match history.list() {
            Ok(records) => records.iter().rev().map(row_from_record).collect(),
            Err(e) => {
                tracing::warn!("failed to list history: {e}");
                Vec::new()
            }
        };
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() {
                ui.global::<State>()
                    .set_history(ModelRc::from(Rc::new(VecModel::from(rows))));
            }
        });
    });
}

fn register_history(ui: &AppWindow, app: &Rc<App>) {
    let logic = ui.global::<Logic>();

    {
        let app = app.clone();
        logic.on_refresh_history(move || refresh_history(&app));
    }

    {
        let app = app.clone();
        logic.on_delete_row(move |id: SharedString| {
            let rt = app.rt.clone();
            let history = app.history.clone();
            let weak = app.weak.clone();
            rt.spawn_blocking(move || {
                match history.delete(id.as_ref()) {
                    Ok(Some(record)) => {
                        reclaim_partial(&record, &engine::storage::temp_dir());
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!("failed to delete history row: {e}"),
                }
                let rows: Vec<HistoryRow> = history
                    .list()
                    .map(|records| records.iter().rev().map(row_from_record).collect())
                    .unwrap_or_default();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.global::<State>()
                            .set_history(ModelRc::from(Rc::new(VecModel::from(rows))));
                    }
                });
            });
        });
    }

    {
        let app = app.clone();
        logic.on_open_row(move |id: SharedString| {
            let rt = app.rt.clone();
            let history = app.history.clone();
            let id = id.to_string();
            rt.spawn_blocking(move || {
                let target = history.list().ok().and_then(|records| {
                    records
                        .iter()
                        .find(|r| r.id == id)
                        .and_then(|r| r.save_path.clone())
                });
                if let Some(target) = target {
                    if let Err(e) = open::that(&target) {
                        tracing::warn!("failed to open {target:?}: {e}");
                    }
                }
            });
        });
    }

    {
        let app = app.clone();
        logic.on_clear_history(move || {
            let rt = app.rt.clone();
            let history = app.history.clone();
            let weak = app.weak.clone();
            rt.spawn_blocking(move || {
                match history.clear() {
                    Ok(removed) => {
                        let temp_dir = engine::storage::temp_dir();
                        for record in &removed {
                            reclaim_partial(record, &temp_dir);
                        }
                    }
                    Err(e) => tracing::warn!("failed to clear history: {e}"),
                }
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.global::<State>()
                            .set_history(ModelRc::from(Rc::new(VecModel::from(
                                Vec::<HistoryRow>::new(),
                            ))));
                        toast(&ui, "History cleared", false);
                    }
                });
            });
        });
    }
}

fn register_settings(ui: &AppWindow, app: &Rc<App>) {
    let logic = ui.global::<Logic>();

    {
        let app = app.clone();
        logic.on_pick_downloads_dir(move || {
            let rt = app.rt.clone();
            let weak = app.weak.clone();
            rt.spawn_blocking(move || {
                let picked = rfd::FileDialog::new()
                    .set_title("Choose downloads folder")
                    .pick_folder();
                if let Some(folder) = picked {
                    let text = folder.to_string_lossy().into_owned();
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
        let app = app.clone();
        logic.on_save_settings(move || {
            let Some(ui) = app.weak.upgrade() else {
                return;
            };
            let state = ui.global::<State>();
            let new_settings = Settings {
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
                history_enabled: state.get_history_enabled(),
            };
            drop(state);

            *app.settings.lock().unwrap() = new_settings.clone();
            let path = app.settings_path.clone();
            let weak = app.weak.clone();
            app.rt
                .spawn_blocking(move || match new_settings.save(&path) {
                    Ok(()) => {
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak.upgrade() {
                                ui.global::<State>().set_settings_status("Saved.".into());
                                toast(&ui, "Settings saved", false);
                            }
                        });
                    }
                    Err(e) => {
                        let msg = format!("Could not save settings: {e}");
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
        let app = app.clone();
        logic.on_page_changed(move |page: SharedString| {
            if page == "history" {
                refresh_history(&app);
            }
        });
    }
}
