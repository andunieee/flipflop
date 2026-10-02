//! The Slint UI and its glue to [`Core`].

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use iroh::EndpointId;
use slint::{ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};
use tokio::runtime::Handle;

use crate::{
    core::{Core, Event, GroupInfo, GroupView, GroupsView, MemberState, Peer, Unseen},
    format, groupdoc,
};

slint::include_modules!();

/// What the UI needs to remember between events. Only touched on the UI thread.
#[derive(Default)]
struct Shared {
    peers: Vec<Peer>,
    groups: GroupsView,
    views: HashMap<String, GroupView>,
    open_group: Option<String>,
    /// What is new in each group (the Home page).
    home: Vec<Unseen>,
    /// The page the UI shows. A group counts as looked at while its page is the one.
    page: String,
    /// The group whose page is showing.
    viewed: Option<String>,
}

type Mirror = Arc<Mutex<Shared>>;
/// Filled once the background start-up has produced a [`Core`]; callbacks do nothing before that.
type Slot = Arc<std::sync::OnceLock<Core>>;

fn model<T: Clone + 'static>(rows: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(rows))
}

pub fn run(data_dir: PathBuf) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let handle = rt.handle().clone();
    let _guard = rt.enter();

    let ui = AppWindow::new()?;
    {
        let state = ui.global::<State>();
        state.set_touch(cfg!(target_os = "android"));
        state.set_live_camera(cfg!(not(target_os = "android")));
    }
    let mirror: Mirror = Arc::default();

    // Core starts in the background: on Android the Bluetooth stack delivers its callbacks through
    // the Java main thread, which only runs while this (native glue) thread keeps pumping the UI.
    let slot: Slot = Arc::default();
    {
        let (slot, weak, mirror) = (slot.clone(), ui.as_weak(), mirror.clone());
        let data_dir = data_dir.clone();
        handle.spawn(async move {
            let want_ble = tokio::task::spawn_blocking(crate::platform::ensure_ble_permission)
                .await
                .unwrap_or(false);
            let events = (weak.clone(), mirror, slot.clone());
            let core = match Core::start(&data_dir, want_ble, move |event| on_event(&events.0, &events.1, &events.2, event)).await {
                Ok(core) => core,
                Err(err) => return set_status(&weak, format!("failed to start: {err:#}")),
            };
            let _ = slot.set(core.clone());
            // `flipflop-cli` hands its commands to us while the window is open.
            #[cfg(not(target_os = "android"))]
            if let Err(err) = crate::ipc::serve(&data_dir, core.clone()) {
                tracing::warn!("command socket: {err:#}");
            }
            core.publish_groups();
            {
                let dir = core.download_dir().display().to_string();
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    let state = ui.global::<State>();
                    state.set_node_ready(true);
                    state.set_download_dir(dir.into());
                });
            }

            // Two BLE statuses share one line: the iroh transport and our discovery.
            let line = Arc::new(Mutex::new((format!("Bluetooth transport {}", core.ble_transport_status()), String::new())));
            let show = {
                let line = line.clone();
                move |weak: &slint::Weak<AppWindow>| {
                    let (t, d) = line.lock().unwrap().clone();
                    let text = if d.is_empty() { t } else { format!("{d} · {t}") };
                    let _ = weak.upgrade_in_event_loop(move |ui| ui.global::<State>().set_ble_status(text.into()));
                }
            };
            show(&weak);
            crate::ble::start(core, move |text| {
                line.lock().unwrap().1 = text;
                show(&weak);
            });
        });
    }

    #[cfg(target_os = "android")]
    {
        let (qr_slot, qr_weak, qr_handle) = (slot.clone(), ui.as_weak(), handle.clone());
        crate::platform::set_qr_handler(move |text| {
            if let Some(core) = qr_slot.get() {
                connect_ticket(core, &qr_weak, &qr_handle, text);
            }
        });
        let (slot, mirror, weak, handle) = (slot.clone(), mirror.clone(), ui.as_weak(), handle.clone());
        // A picked file is offered to whichever group is open.
        crate::platform::set_file_handler(move |kind, path, name| {
            let Some(core) = slot.get().cloned() else { return };
            let Some(gid) = open_group_id(&mirror) else { return };
            // A content:// URI is shared where it is; anything else is our own copy.
            let staged = !path.starts_with("content://");
            offer(&core, &weak, &handle, gid, PathBuf::from(path), Some(name).filter(|n| !n.is_empty()), &kind, staged);
        });
    }

    let logic = ui.global::<Logic>();

    logic.on_rename({
        let slot = slot.clone();
        move |name| {
            if let Some(core) = slot.get() {
                core.set_name(&name);
            }
        }
    });

    logic.on_connect({
        let (slot, weak, handle) = (slot.clone(), ui.as_weak(), handle.clone());
        move |id| {
            let (Some(core), Ok(id)) = (slot.get(), id.parse::<EndpointId>()) else { return };
            let (core, weak) = (core.clone(), weak.clone());
            set_status(&weak, "Connecting…");
            handle.spawn(async move {
                match core.connect(id).await {
                    Ok(()) => set_status(&weak, "Connected"),
                    Err(err) => set_status(&weak, format!("{err:#}")),
                }
            });
        }
    });

    logic.on_connect_ticket({
        let (slot, weak, handle) = (slot.clone(), ui.as_weak(), handle.clone());
        move |text| {
            if let Some(core) = slot.get() {
                connect_ticket(core, &weak, &handle, text.to_string());
            }
        }
    });

    logic.on_new_group({
        let (slot, weak, mirror) = (slot.clone(), ui.as_weak(), mirror.clone());
        move || {
            let Some(core) = slot.get() else { return };
            let id = core.create_group();
            open_group(&weak, &mirror, &slot, Some(id));
        }
    });

    logic.on_open_group({
        let (weak, mirror, slot) = (ui.as_weak(), mirror.clone(), slot.clone());
        move |id| open_group(&weak, &mirror, &slot, Some(id.to_string()))
    });

    logic.on_close_group({
        let (weak, mirror, slot) = (ui.as_weak(), mirror.clone(), slot.clone());
        move || open_group(&weak, &mirror, &slot, None)
    });

    logic.on_add_to_group({
        let slot = slot.clone();
        move |group, peer| {
            if let (Some(core), Ok(peer)) = (slot.get(), peer.parse::<EndpointId>()) {
                core.add_to_group(&group, peer);
            }
        }
    });

    logic.on_pair_peer({
        let slot = slot.clone();
        move |id| {
            if let (Some(core), Ok(id)) = (slot.get(), id.parse::<EndpointId>()) {
                core.pair(id);
            }
        }
    });

    logic.on_unpair_peer({
        let slot = slot.clone();
        move |id| {
            if let (Some(core), Ok(id)) = (slot.get(), id.parse::<EndpointId>()) {
                core.unpair(id);
            }
        }
    });

    logic.on_invite_peer_to({
        let (slot, weak, mirror) = (slot.clone(), ui.as_weak(), mirror.clone());
        move |index| {
            let Some(core) = slot.get() else { return };
            let picked = {
                let shared = mirror.lock().unwrap();
                let peer = shared.open_group.as_ref().and_then(|id| shared.groups.groups.iter().find(|g| g.id == *id)).and_then(|g| g.with);
                let group = usize::try_from(index).ok().and_then(|i| invitable_groups(&shared).get(i).map(|g| (g.id.clone(), group_title(g))));
                peer.zip(group)
            };
            let Some((peer, (group, title))) = picked else { return };
            core.add_to_group(&group, peer);
            set_status(&weak, format!("Invited to {title}"));
        }
    });

    logic.on_mark_all_seen({
        let slot = slot.clone();
        move || {
            if let Some(core) = slot.get() {
                core.mark_all_seen();
            }
        }
    });

    logic.on_page_changed({
        let (slot, mirror) = (slot.clone(), mirror.clone());
        move |page| {
            let open = {
                let mut shared = mirror.lock().unwrap();
                shared.page = page.to_string();
                shared.open_group.clone().filter(|_| page == "group")
            };
            looking_at(&mirror, &slot, open);
        }
    });

    logic.on_accept_invite({
        let slot = slot.clone();
        move |inviter, group| {
            if let (Some(core), Ok(inviter)) = (slot.get(), inviter.parse::<EndpointId>()) {
                core.accept_invite(inviter, &group);
            }
        }
    });

    logic.on_reject_invite({
        let slot = slot.clone();
        move |inviter, group| {
            if let (Some(core), Ok(inviter)) = (slot.get(), inviter.parse::<EndpointId>()) {
                core.reject_invite(inviter, &group);
            }
        }
    });

    logic.on_save_edit({
        let slot = slot.clone();
        move |kind, id, text| {
            let Some(core) = slot.get() else { return };
            match kind.as_str() {
                "peer" => {
                    if let Ok(id) = id.parse::<EndpointId>() {
                        core.rename_contact(id, &text);
                    }
                }
                "group" => core.rename_group(&id, &text),
                _ => {}
            }
        }
    });

    logic.on_delete_edit({
        let (slot, weak, mirror) = (slot.clone(), ui.as_weak(), mirror.clone());
        move |kind, id| {
            let Some(core) = slot.get() else { return };
            match kind.as_str() {
                "peer" => {
                    if let Ok(id) = id.parse::<EndpointId>() {
                        core.forget_contact(id);
                        core.disconnect(id);
                    }
                }
                "group" => {
                    open_group(&weak, &mirror, &slot, None);
                    core.delete_group(&id);
                }
                _ => {}
            }
        }
    });

    // ---- group content ----------------------------------------------------------------

    logic.on_send_text({
        let (slot, weak, mirror) = (slot.clone(), ui.as_weak(), mirror.clone());
        move |text| {
            let (Some(core), Some(gid)) = (slot.get(), open_group_id(&mirror)) else { return };
            if let Err(err) = core.send_text(&gid, &text) {
                set_status(&weak, format!("{err:#}"));
            }
        }
    });

    logic.on_delete_text({
        let (slot, weak, mirror) = (slot.clone(), ui.as_weak(), mirror.clone());
        move |id| {
            let (Some(core), Some(gid)) = (slot.get(), open_group_id(&mirror)) else { return };
            if let Err(err) = core.delete_text(&gid, &id) {
                set_status(&weak, format!("{err:#}"));
            }
        }
    });

    logic.on_copy_text({
        let weak = ui.as_weak();
        move |text| {
            crate::platform::copy_text(&text);
            set_status(&weak, "Copied");
        }
    });

    logic.on_paste(|| crate::platform::paste_text().into());

    logic.on_open_link(|url| crate::platform::open_url(url.trim()));

    logic.on_pad_edited({
        let (slot, mirror) = (slot.clone(), mirror.clone());
        move |text| {
            if let (Some(core), Some(gid)) = (slot.get(), open_group_id(&mirror)) {
                core.set_scratch(&gid, &text);
            }
        }
    });

    logic.on_paste_pad({
        let (slot, mirror, weak) = (slot.clone(), mirror.clone(), ui.as_weak());
        move || {
            let (Some(core), Some(gid), Some(ui)) = (slot.get(), open_group_id(&mirror), weak.upgrade()) else { return };
            let pasted = crate::platform::paste_text();
            if pasted.is_empty() {
                return;
            }
            let state = ui.global::<State>();
            let text = state.get_pad_text().to_string();
            let at = if state.get_pad_focused() { (state.get_pad_cursor() as usize).min(text.len()) } else { text.len() };
            let at = (0..=at).rev().find(|i| text.is_char_boundary(*i)).unwrap_or(0);
            let new = format!("{}{}{}", &text[..at], pasted, &text[at..]);
            state.set_pad_text(new.clone().into());
            state.set_pad_set_cursor((at + pasted.len()) as i32);
            core.set_scratch(&gid, &new);
        }
    });

    logic.on_pick({
        let (slot, mirror, weak, handle) = (slot.clone(), mirror.clone(), ui.as_weak(), handle.clone());
        move |kind| {
            #[cfg(target_os = "android")]
            {
                let _ = (&slot, &mirror, &handle);
                if kind.as_str() == "app" {
                    let apps: Vec<AppRow> = crate::platform::list_apps()
                        .into_iter()
                        .map(|(path, name, detail)| AppRow { path: path.into(), name: name.into(), detail: detail.into() })
                        .collect();
                    if let Some(ui) = weak.upgrade() {
                        let state = ui.global::<State>();
                        state.set_apps(model(apps));
                        state.set_app_picker_open(true);
                    }
                } else {
                    crate::platform::pick_files(&kind);
                }
            }
            #[cfg(not(target_os = "android"))]
            {
                let (Some(core), Some(gid)) = (slot.get().cloned(), open_group_id(&mirror)) else { return };
                let (weak, handle2) = (weak.clone(), handle.clone());
                let media = kind.as_str() == "media";
                handle.spawn(async move {
                    let mut dialog = rfd::AsyncFileDialog::new().set_title("Offer to the group");
                    if media {
                        dialog = dialog
                            .add_filter("Photos and videos", &["jpg", "jpeg", "png", "gif", "webp", "heic", "mp4", "mkv", "mov", "webm"]);
                    }
                    for file in dialog.pick_files().await.unwrap_or_default() {
                        let kind = if media { "media" } else { "file" };
                        offer(&core, &weak, &handle2, gid.clone(), file.path().to_owned(), None, kind, false);
                    }
                });
            }
        }
    });

    logic.on_share_app({
        let (slot, mirror, weak, handle) = (slot.clone(), mirror.clone(), ui.as_weak(), handle.clone());
        move |path| {
            let (Some(core), Some(gid)) = (slot.get().cloned(), open_group_id(&mirror)) else { return };
            let name = weak.upgrade().and_then(|ui| {
                let apps = ui.global::<State>().get_apps();
                (0..apps.row_count()).filter_map(|i| apps.row_data(i)).find(|a| a.path == path).map(|a| format!("{}.apk", a.name))
            });
            offer(&core, &weak, &handle, gid, PathBuf::from(path.as_str()), name, "apk", false);
        }
    });

    logic.on_stop_file({
        let (slot, weak, mirror) = (slot.clone(), ui.as_weak(), mirror.clone());
        move |id| {
            let (Some(core), Some(gid)) = (slot.get(), open_group_id(&mirror)) else { return };
            if let Err(err) = core.stop_file(&gid, &id) {
                set_status(&weak, format!("{err:#}"));
            }
        }
    });

    logic.on_download_file({
        let (slot, weak, mirror, handle) = (slot.clone(), ui.as_weak(), mirror.clone(), handle.clone());
        move |id| {
            let (Some(core), Some(gid)) = (slot.get().cloned(), open_group_id(&mirror)) else { return };
            let weak = weak.clone();
            handle.spawn(async move {
                if let Err(err) = core.download_file(&gid, &id).await {
                    set_status(&weak, format!("{err:#}"));
                }
            });
        }
    });

    logic.on_set_auto_download({
        let (slot, mirror) = (slot.clone(), mirror.clone());
        move |on| {
            if let (Some(core), Some(gid)) = (slot.get(), open_group_id(&mirror)) {
                core.set_group_settings(&gid, Some(on), None);
            }
        }
    });

    logic.on_set_auto_stop({
        let (slot, mirror) = (slot.clone(), mirror.clone());
        move |on| {
            if let (Some(core), Some(gid)) = (slot.get(), open_group_id(&mirror)) {
                core.set_group_settings(&gid, None, Some(on));
            }
        }
    });

    // QR scanning: webcam on desktop, system scanner activity on Android.
    #[cfg(not(target_os = "android"))]
    {
        use std::{cell::RefCell, rc::Rc};
        let scanner: Rc<RefCell<Option<crate::scan::Scanner>>> = Rc::default();
        logic.on_scan({
            let (slot, weak, handle, scanner) = (slot.clone(), ui.as_weak(), handle.clone(), scanner.clone());
            move || {
                let Some(core) = slot.get().cloned() else { return };
                let (frame_weak, done_weak) = (weak.clone(), weak.clone());
                let handle = handle.clone();
                if let Some(ui) = weak.upgrade() {
                    ui.global::<State>().set_scanning(true);
                }
                *scanner.borrow_mut() = Some(crate::scan::start(
                    move |w, h, rgba| {
                        let _ = frame_weak.upgrade_in_event_loop(move |ui| {
                            let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&rgba, w, h);
                            ui.global::<State>().set_camera_frame(Image::from_rgba8(buf));
                        });
                    },
                    move |result| {
                        let _ = done_weak.clone().upgrade_in_event_loop(move |ui| ui.global::<State>().set_scanning(false));
                        match result {
                            Ok(text) => connect_ticket(&core, &done_weak, &handle, text),
                            Err(msg) if msg == "cancelled" => {}
                            Err(msg) => set_status(&done_weak, msg),
                        }
                    },
                ));
            }
        });
        logic.on_cancel_scan({
            let (weak, scanner) = (ui.as_weak(), scanner.clone());
            move || {
                scanner.borrow_mut().take();
                if let Some(ui) = weak.upgrade() {
                    ui.global::<State>().set_scanning(false);
                }
            }
        });
    }
    #[cfg(target_os = "android")]
    {
        logic.on_scan(crate::platform::start_qr_scan);
        logic.on_cancel_scan(|| {});
    }

    ui.run()?;
    if let Some(core) = slot.get() {
        rt.block_on(core.shutdown());
    }
    Ok(())
}

fn open_group_id(mirror: &Mirror) -> Option<String> {
    mirror.lock().unwrap().open_group.clone()
}

/// Offer a file to a group from a background task and report how it went.
#[allow(clippy::too_many_arguments)]
fn offer(core: &Core, weak: &slint::Weak<AppWindow>, handle: &Handle, gid: String, path: PathBuf, name: Option<String>, kind: &str, staged: bool) {
    let (core, weak, kind) = (core.clone(), weak.clone(), kind.to_owned());
    handle.spawn(async move {
        match core.offer_file(&gid, path, name, &kind, staged).await {
            Ok(_) => set_status(&weak, "Offered to the group"),
            Err(err) => set_status(&weak, format!("{err:#}")),
        }
    });
}

fn connect_ticket(core: &Core, weak: &slint::Weak<AppWindow>, handle: &Handle, text: String) {
    let (core, weak) = (core.clone(), weak.clone());
    set_status(&weak, "Connecting…");
    handle.spawn(async move {
        match core.connect_ticket(&text).await {
            Ok(_) => {
                let _ = weak.upgrade_in_event_loop(|ui| {
                    let state = ui.global::<State>();
                    state.set_page("peers".into());
                    state.set_status("Connected".into());
                });
            }
            Err(err) => set_status(&weak, format!("{err:#}")),
        }
    });
}

fn set_status(weak: &slint::Weak<AppWindow>, text: impl Into<String>) {
    let text = text.into();
    let _ = weak.upgrade_in_event_loop(move |ui| ui.global::<State>().set_status(text.into()));
}

/// Open a group's page, or go back to the list (`None`).
fn open_group(weak: &slint::Weak<AppWindow>, mirror: &Mirror, slot: &Slot, id: Option<String>) {
    let Some(ui) = weak.upgrade() else { return };
    let state = ui.global::<State>();
    let page = {
        let mut shared = mirror.lock().unwrap();
        shared.open_group = id.clone();
        state.set_open_group(id.clone().unwrap_or_default().into());
        match &id {
            Some(id) => {
                let implied = shared.groups.groups.iter().any(|g| g.id == *id && g.with.is_some());
                // A peer's page starts with the peer; a group's with its files.
                state.set_group_tab(if implied { "info" } else { "files" }.into());
                state.set_invite_choice(0);
                show_group(&ui, &shared, true);
                "group"
            }
            None => list_page(&state),
        }
    };
    state.set_page(page.into());
    // Going from one group straight to another does not change the page.
    looking_at(mirror, slot, id);
}

/// The group the user is looking at changed (`None`: none): the one they left is caught up, so is the
/// new one.
fn looking_at(mirror: &Mirror, slot: &Slot, now: Option<String>) {
    let left = std::mem::replace(&mut mirror.lock().unwrap().viewed, now.clone());
    if let Some(core) = slot.get() {
        for gid in left.into_iter().chain(now) {
            core.mark_seen(&gid);
        }
    }
}

/// Where leaving a group goes: the groups list on phones, Home on the desktop (which has no list page).
fn list_page(state: &State) -> &'static str {
    if state.get_touch() { "groups" } else { "home" }
}

/// Called from tokio threads.
fn on_event(weak: &slint::Weak<AppWindow>, mirror: &Mirror, slot: &Slot, event: Event) {
    match event {
        Event::Name(name) => {
            let _ = weak.upgrade_in_event_loop(move |ui| ui.global::<State>().set_my_name(name.into()));
        }
        Event::Notice(text) => set_status(weak, text),
        Event::Ticket(ticket) => {
            let qr = crate::qr::render(&ticket, 6);
            let _ = weak.upgrade_in_event_loop(move |ui| {
                let state = ui.global::<State>();
                if let Some((w, h, rgba)) = qr {
                    let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&rgba, w, h);
                    state.set_qr(Image::from_rgba8(buf));
                }
                state.set_ticket(ticket.into());
            });
        }
        Event::Peers(peers) => {
            let mirror = mirror.clone();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.global::<State>().set_peers(model(peers.iter().map(peer_row).collect()));
                let mut shared = mirror.lock().unwrap();
                shared.peers = peers;
                show_groups(&ui, &shared);
                show_group(&ui, &shared, false);
            });
        }
        Event::Groups(view) => {
            let mirror = mirror.clone();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                let mut shared = mirror.lock().unwrap();
                shared.groups = view;
                let ids: Vec<String> = shared.groups.groups.iter().map(|g| g.id.clone()).collect();
                shared.views.retain(|id, _| ids.contains(id));
                if shared.open_group.as_ref().is_some_and(|id| !ids.contains(id)) {
                    shared.open_group = None;
                    let state = ui.global::<State>();
                    state.set_open_group(SharedString::default());
                    state.set_page(list_page(&state).into());
                }
                show_groups(&ui, &shared);
                show_group(&ui, &shared, false);
            });
        }
        Event::Group(view) => {
            let (mirror, slot) = (mirror.clone(), slot.clone());
            let _ = weak.upgrade_in_event_loop(move |ui| {
                let mut shared = mirror.lock().unwrap();
                let opened = shared.open_group.as_deref() == Some(view.id.as_str());
                // Our own scratchpad edits are not news; they are caught up when the page is left.
                let looking = opened && shared.viewed.as_deref() == Some(view.id.as_str()) && !view.scratch_local;
                let gid = view.id.clone();
                shared.views.insert(view.id.clone(), view);
                show_groups(&ui, &shared);
                if opened {
                    show_group(&ui, &shared, false);
                }
                drop(shared);
                if let (true, Some(core)) = (looking, slot.get()) {
                    core.mark_seen(&gid);
                }
            });
        }
        Event::Home(news) => {
            let mirror = mirror.clone();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                let mut shared = mirror.lock().unwrap();
                shared.home = news;
                show_groups(&ui, &shared);
            });
        }
        Event::Message { .. } => {}
    }
}

fn peer_row(p: &Peer) -> PeerRow {
    PeerRow {
        id: p.id.to_string().into(),
        title: p.alias.clone().unwrap_or_else(|| p.name.clone()).into(),
        subtitle: if p.alias.is_some() { p.name.clone().into() } else { SharedString::default() },
        alias: p.alias.clone().unwrap_or_default().into(),
        short: p.id.fmt_short().to_string().into(),
        initials: format::initials(p.alias.as_ref().unwrap_or(&p.name)).into(),
        lan: p.lan,
        ble: p.ble,
        connected: p.connected,
        saved: p.saved,
        paired: p.paired,
        paired_us: p.paired_us,
        group_id: p.pair_group.clone().unwrap_or_default().into(),
    }
}

/// An implied group is named after the other device.
fn group_title(g: &GroupInfo) -> String {
    match g.with.and_then(|w| g.members.iter().find(|m| m.id == w)) {
        Some(m) => m.name.clone(),
        None => g.alias.clone().unwrap_or_else(|| g.haiku.clone()),
    }
}

/// The groups list and the invites.
fn show_groups(ui: &AppWindow, shared: &Shared) {
    let rows: Vec<GroupRow> = shared
        .groups
        .groups
        .iter()
        .filter(|g| g.with.is_none())
        .map(|g| {
            let view = shared.views.get(&g.id);
            let joined = view.map_or(1, |v| v.members.len().max(1));
            let others_online = view.is_some_and(|v| v.members.iter().any(|m| m.online && !m.me));
            let waiting = g.members.iter().filter(|m| matches!(m.state, MemberState::Pending | MemberState::Sent)).count();
            let mut summary = format!("{joined} member{}", if joined == 1 { "" } else { "s" });
            if waiting > 0 {
                summary += &format!(" · {waiting} invited");
            }
            if let Some(v) = view {
                let files = v.files.iter().filter(|f| !f.stopped).count();
                if files > 0 {
                    summary += &format!(" · {files} file{} on offer", if files == 1 { "" } else { "s" });
                }
            }
            GroupRow {
                id: g.id.clone().into(),
                title: group_title(g).into(),
                subtitle: if g.alias.is_some() { g.haiku.clone().into() } else { SharedString::default() },
                summary: summary.into(),
                online: others_online,
            }
        })
        .collect();
    let invites: Vec<InviteRow> = shared
        .groups
        .invites
        .iter()
        .map(|i| InviteRow {
            inviter_id: i.inviter.to_string().into(),
            group_id: i.group.clone().into(),
            inviter_name: i.inviter_name.clone().into(),
            group_title: i.haiku.clone().into(),
        })
        .collect();
    let state = ui.global::<State>();
    state.set_groups(model(rows));
    state.set_invites(model(invites));
    state.set_home(model(
        shared
            .home
            .iter()
            .map(|n| HomeRow {
                group_id: n.group.clone().into(),
                title: n.title.clone().into(),
                lines: n.lines().join("\n").into(),
                paired: n.with.is_some(),
            })
            .collect(),
    ));
}

/// The groups the open peer page offers to invite the peer to, in the order of the dropdown: the
/// listed groups the peer is not part of (a declined invite can be sent again).
fn invitable_groups(shared: &Shared) -> Vec<&GroupInfo> {
    let Some(peer) = shared.open_group.as_ref().and_then(|id| shared.groups.groups.iter().find(|g| g.id == *id)).and_then(|g| g.with) else {
        return Vec::new();
    };
    let key = peer.to_string();
    shared
        .groups
        .groups
        .iter()
        .filter(|g| g.with.is_none())
        .filter(|g| !g.members.iter().any(|m| m.id == peer && m.state != MemberState::Declined))
        .filter(|g| !shared.views.get(&g.id).is_some_and(|v| v.members.iter().any(|m| m.id == key)))
        .collect()
}

/// Fill the open group's page. `fresh`: the group was just opened, so the scratchpad is loaded
/// whatever it held before.
fn show_group(ui: &AppWindow, shared: &Shared, fresh: bool) {
    let state = ui.global::<State>();
    let Some(info) = shared.open_group.as_ref().and_then(|id| shared.groups.groups.iter().find(|g| g.id == *id)) else {
        return;
    };
    state.set_group_title(group_title(info).into());
    state.set_group_haiku(info.haiku.clone().into());
    state.set_group_alias(info.alias.clone().unwrap_or_default().into());
    state.set_group_implied(info.with.is_some());
    let peer = info.with.and_then(|w| shared.peers.iter().find(|p| p.id == w));
    if let Some(p) = peer {
        state.set_group_peer(peer_row(p));
    }
    state.set_auto_download(info.auto_download);
    state.set_auto_stop(info.auto_stop);
    let view = shared.views.get(&info.id);

    // Members: who the document lists, then invitees that have not joined.
    let mut members: Vec<MemberRow> = view
        .map(|v| {
            v.members
                .iter()
                .map(|m| MemberRow {
                    id: m.id.clone().into(),
                    name: m.name.clone().into(),
                    initials: format::initials(&m.name).into(),
                    status: if m.me { "this device" } else if m.online { "online" } else { "offline" }.into(),
                    tone: if m.me || m.online { "ok" } else { "muted" }.into(),
                    online: m.online,
                    me: m.me,
                })
                .collect()
        })
        .unwrap_or_default();
    for m in &info.members {
        if members.iter().any(|row| row.id.as_str() == m.id.to_string()) {
            continue;
        }
        let (status, tone) = match m.state {
            MemberState::Accepted => ("joined", "ok"),
            MemberState::Sent => ("invited", "active"),
            MemberState::Pending => ("waiting for them to be online", "warn"),
            MemberState::Declined => ("declined", "error"),
        };
        members.push(MemberRow {
            id: m.id.to_string().into(),
            name: m.name.clone().into(),
            initials: format::initials(&m.name).into(),
            status: status.into(),
            tone: tone.into(),
            online: false,
            me: false,
        });
    }
    // Members the document lists (e.g. everyone but the inviter, after accepting) are not candidates.
    state.set_invitable(model(invitable_groups(shared).iter().map(|g| SharedString::from(group_title(g))).collect()));
    state.set_candidates(model(
        info.candidates
            .iter()
            .filter(|(id, _)| !members.iter().any(|row| row.id.as_str() == id.to_string()))
            .map(|(id, name)| CandidateRow { id: id.to_string().into(), name: name.clone().into() })
            .collect(),
    ));
    state.set_members(model(members));

    let Some(view) = view else { return };
    let files: Vec<FileRow> = view
        .files
        .iter()
        .map(|f| {
            let downloading = f.progress.is_some();
            let (done, total) = f.progress.unwrap_or((0, f.size));
            FileRow {
                id: f.id.clone().into(),
                name: f.name.clone().into(),
                size: format::size(f.size).into(),
                meta: format!("{} · {}", if f.mine { "you".to_string() } else { f.from_name.clone() }, format::time(f.ts)).into(),
                kind: match f.kind.as_str() {
                    "media" => 1,
                    "apk" => 2,
                    _ => 0,
                },
                mine: f.mine,
                have: f.have,
                stopped: f.stopped,
                can_download: !f.mine && !f.have && !f.stopped && !downloading && f.offerer_online,
                downloading,
                progress: if total == 0 { 0.0 } else { done as f32 / total as f32 },
                progress_label: format!("{} of {}", format::size(done), format::size(total)).into(),
                audience: if f.audience == 0 {
                    SharedString::default()
                } else {
                    format!("{} of {} have it", f.downloaded_by.len(), f.audience).into()
                },
                saved_to: f.saved_to.as_ref().map(|p| p.display().to_string()).unwrap_or_default().into(),
                error: f.error.clone().unwrap_or_default().into(),
            }
        })
        .collect();
    state.set_files(model(files));
    let texts: Vec<TextRow> = view
        .texts
        .iter()
        .map(|t| TextRow {
            id: t.id.clone().into(),
            name: t.name.clone().into(),
            text: t.text.clone().into(),
            time: format::time(t.ts).into(),
            mine: t.mine,
            is_link: format::is_link(&t.text),
        })
        .collect();
    state.set_texts(model(texts));

    // The scratchpad: our own edits are already on screen; someone else's keep the cursor in place.
    let shown = state.get_pad_text().to_string();
    if fresh {
        state.set_pad_text(view.scratch.clone().into());
    } else if !view.scratch_local && shown != view.scratch {
        let to_char = |text: &str, byte: i32| text[..(byte.max(0) as usize).min(text.len())].chars().count();
        let to_byte = |text: &str, chars: usize| text.char_indices().nth(chars).map_or(text.len(), |(i, _)| i);
        let cursor = groupdoc::map_cursor(&shown, &view.scratch, to_char(&shown, state.get_pad_cursor()));
        let anchor = groupdoc::map_cursor(&shown, &view.scratch, to_char(&shown, state.get_pad_anchor()));
        let focused = state.get_pad_focused();
        state.set_pad_text(view.scratch.clone().into());
        if focused {
            state.set_pad_anchor_to(to_byte(&view.scratch, anchor) as i32);
            state.set_pad_set_cursor(to_byte(&view.scratch, cursor) as i32);
        }
    }
}
