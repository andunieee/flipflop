//! The Slint UI and its glue to [`Core`].

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use iroh::EndpointId;
use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};
use tokio::runtime::Handle;

use crate::core::{Core, Event, GroupInfo, GroupsView, MemberState, Peer};

slint::include_modules!();

#[derive(Default)]
struct UiState {
    chats: HashMap<EndpointId, Vec<ChatLine>>,
    peers: Vec<Peer>,
    selected: Option<EndpointId>,
    groups: GroupsView,
    open_group: Option<String>,
}

type State = Arc<Mutex<UiState>>;
/// Filled once the background start-up has produced a [`Core`]; callbacks do nothing before that.
type Slot = Arc<std::sync::OnceLock<Core>>;

pub fn run(data_dir: PathBuf) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let handle = rt.handle().clone();
    let _guard = rt.enter();

    let ui = MainWindow::new()?;
    ui.set_live_camera(cfg!(not(target_os = "android")));
    let state: State = Arc::default();

    // Core starts in the background: on Android the Bluetooth stack delivers its callbacks through
    // the Java main thread, which only runs while this (native glue) thread keeps pumping the UI.
    let slot: Slot = Arc::default();
    {
        let (slot, weak, state) = (slot.clone(), ui.as_weak(), state.clone());
        let data_dir = data_dir.clone();
        handle.spawn(async move {
            let want_ble = tokio::task::spawn_blocking(crate::platform::ensure_ble_permission)
                .await
                .unwrap_or(false);
            let events = (weak.clone(), state);
            let core = match Core::start(&data_dir, want_ble, move |event| on_event(&events.0, &events.1, event)).await {
                Ok(core) => core,
                Err(err) => return set_status(&weak, format!("failed to start: {err:#}")),
            };
            let _ = slot.set(core.clone());

            // Two BLE statuses share one line: the iroh transport (chat over Bluetooth) and our discovery.
            let line = Arc::new(Mutex::new((format!("transport {}", core.ble_transport_status()), String::new())));
            let show = {
                let line = line.clone();
                move |weak: &slint::Weak<MainWindow>| {
                    let (t, d) = line.lock().unwrap().clone();
                    let text = if d.is_empty() { t } else { format!("{d} · {t}") };
                    let _ = weak.upgrade_in_event_loop(move |ui| ui.set_ble_status(text.into()));
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
        let (slot, weak, handle) = (slot.clone(), ui.as_weak(), handle.clone());
        crate::platform::set_qr_handler(move |text| {
            if let Some(core) = slot.get() {
                connect_ticket(core, &weak, &handle, text);
            }
        });
    }

    ui.on_rename({
        let slot = slot.clone();
        move |name| {
            if let Some(core) = slot.get() {
                core.set_name(&name);
            }
        }
    });

    ui.on_connect({
        let (slot, weak, handle) = (slot.clone(), ui.as_weak(), handle.clone());
        move |id| {
            let (Some(core), Ok(id)) = (slot.get(), id.parse::<EndpointId>()) else { return };
            let (core, weak) = (core.clone(), weak.clone());
            set_status(&weak, "connecting…");
            handle.spawn(async move {
                match core.connect(id).await {
                    Ok(()) => set_status(&weak, "connected"),
                    Err(err) => set_status(&weak, format!("{err:#}")),
                }
            });
        }
    });

    ui.on_new_group({
        let (slot, weak, state) = (slot.clone(), ui.as_weak(), state.clone());
        move || {
            let Some(core) = slot.get() else { return };
            let id = core.create_group();
            let mut st = state.lock().unwrap();
            st.open_group = Some(id);
            if let Some(ui) = weak.upgrade() {
                show_group(&ui, &st);
            }
        }
    });

    ui.on_open_group_id({
        let (weak, state) = (ui.as_weak(), state.clone());
        move |id| {
            let mut st = state.lock().unwrap();
            st.open_group = Some(id.to_string());
            if let Some(ui) = weak.upgrade() {
                show_group(&ui, &st);
            }
        }
    });

    ui.on_close_group({
        let (weak, state) = (ui.as_weak(), state.clone());
        move || {
            let mut st = state.lock().unwrap();
            st.open_group = None;
            if let Some(ui) = weak.upgrade() {
                show_group(&ui, &st);
            }
        }
    });

    ui.on_add_to_group({
        let slot = slot.clone();
        move |group, peer| {
            if let (Some(core), Ok(peer)) = (slot.get(), peer.parse::<EndpointId>()) {
                core.add_to_group(&group, peer);
            }
        }
    });

    ui.on_accept_invite({
        let slot = slot.clone();
        move |inviter, group| {
            if let (Some(core), Ok(inviter)) = (slot.get(), inviter.parse::<EndpointId>()) {
                core.accept_invite(inviter, &group);
            }
        }
    });

    ui.on_reject_invite({
        let slot = slot.clone();
        move |inviter, group| {
            if let (Some(core), Ok(inviter)) = (slot.get(), inviter.parse::<EndpointId>()) {
                core.reject_invite(inviter, &group);
            }
        }
    });

    ui.on_save_edit({
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

    ui.on_delete_edit({
        let (slot, weak, state) = (slot.clone(), ui.as_weak(), state.clone());
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
                    core.delete_group(&id);
                    let mut st = state.lock().unwrap();
                    st.open_group = None;
                    if let Some(ui) = weak.upgrade() {
                        show_group(&ui, &st);
                    }
                }
                _ => {}
            }
        }
    });

    ui.on_connect_ticket({
        let (slot, weak, handle) = (slot.clone(), ui.as_weak(), handle.clone());
        move |text| {
            if let Some(core) = slot.get() {
                connect_ticket(core, &weak, &handle, text.to_string());
            }
        }
    });

    ui.on_open_chat({
        let (weak, state) = (ui.as_weak(), state.clone());
        move |id| {
            let Ok(id) = id.parse::<EndpointId>() else { return };
            let mut st = state.lock().unwrap();
            st.selected = Some(id);
            if let Some(ui) = weak.upgrade() {
                show_chat(&ui, &st);
            }
        }
    });

    ui.on_close_chat({
        let (weak, state) = (ui.as_weak(), state.clone());
        move || {
            let mut st = state.lock().unwrap();
            st.selected = None;
            if let Some(ui) = weak.upgrade() {
                show_chat(&ui, &st);
            }
        }
    });

    ui.on_send({
        let (slot, weak, state, handle) = (slot.clone(), ui.as_weak(), state.clone(), handle.clone());
        move |text| {
            let text = text.trim().to_string();
            let (Some(core), Some(id)) = (slot.get(), state.lock().unwrap().selected) else { return };
            if text.is_empty() {
                return;
            }
            let (core, weak) = (core.clone(), weak.clone());
            handle.spawn(async move {
                if let Err(err) = core.send(id, text).await {
                    set_status(&weak, format!("send failed: {err:#}"));
                }
            });
        }
    });

    // QR scanning: webcam on desktop, system scanner activity on Android.
    #[cfg(not(target_os = "android"))]
    {
        use std::{cell::RefCell, rc::Rc};
        let scanner: Rc<RefCell<Option<crate::scan::Scanner>>> = Rc::default();
        ui.on_scan({
            let (slot, weak, handle, scanner) = (slot.clone(), ui.as_weak(), handle.clone(), scanner.clone());
            move || {
                let Some(core) = slot.get().cloned() else { return };
                let (frame_weak, done_weak) = (weak.clone(), weak.clone());
                let handle = handle.clone();
                if let Some(ui) = weak.upgrade() {
                    ui.set_scanning(true);
                }
                *scanner.borrow_mut() = Some(crate::scan::start(
                    move |w, h, rgba| {
                        let _ = frame_weak.upgrade_in_event_loop(move |ui| {
                            let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&rgba, w, h);
                            ui.set_camera_frame(Image::from_rgba8(buf));
                        });
                    },
                    move |result| {
                        let _ = done_weak.clone().upgrade_in_event_loop(move |ui| ui.set_scanning(false));
                        match result {
                            Ok(text) => connect_ticket(&core, &done_weak, &handle, text),
                            Err(msg) if msg == "cancelled" => {}
                            Err(msg) => set_status(&done_weak, msg),
                        }
                    },
                ));
            }
        });
        ui.on_cancel_scan({
            let (weak, scanner) = (ui.as_weak(), scanner.clone());
            move || {
                scanner.borrow_mut().take();
                if let Some(ui) = weak.upgrade() {
                    ui.set_scanning(false);
                }
            }
        });
    }
    #[cfg(target_os = "android")]
    {
        ui.on_scan(crate::platform::start_qr_scan);
        ui.on_cancel_scan(|| {});
    }

    ui.run()?;
    if let Some(core) = slot.get() {
        rt.block_on(core.shutdown());
    }
    Ok(())
}

fn connect_ticket(core: &Core, weak: &slint::Weak<MainWindow>, handle: &Handle, text: String) {
    let (core, weak) = (core.clone(), weak.clone());
    set_status(&weak, "connecting…");
    handle.spawn(async move {
        match core.connect_ticket(&text).await {
            Ok(_) => {
                let _ = weak.upgrade_in_event_loop(|ui| {
                    ui.set_tab(0);
                    ui.set_status("connected".into());
                });
            }
            Err(err) => set_status(&weak, format!("{err:#}")),
        }
    });
}

fn set_status(weak: &slint::Weak<MainWindow>, text: impl Into<String>) {
    let text = text.into();
    let _ = weak.upgrade_in_event_loop(move |ui| ui.set_status(text.into()));
}

/// Called from tokio threads.
fn on_event(weak: &slint::Weak<MainWindow>, state: &State, event: Event) {
    match event {
        Event::Name(name) => {
            let _ = weak.upgrade_in_event_loop(move |ui| ui.set_my_name(name.into()));
        }
        Event::Notice(text) => set_status(weak, text),
        Event::Ticket(ticket) => {
            let qr = crate::qr::render(&ticket, 6);
            let _ = weak.upgrade_in_event_loop(move |ui| {
                if let Some((w, h, rgba)) = qr {
                    let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&rgba, w, h);
                    ui.set_qr(Image::from_rgba8(buf));
                }
                ui.set_ticket(ticket.into());
            });
        }
        Event::Peers(peers) => {
            let items: Vec<PeerItem> = peers
                .iter()
                .map(|p| PeerItem {
                    id: p.id.to_string().into(),
                    title: p.alias.clone().unwrap_or_else(|| p.name.clone()).into(),
                    subtitle: if p.alias.is_some() { p.name.clone().into() } else { SharedString::default() },
                    alias: p.alias.clone().unwrap_or_default().into(),
                    short: p.id.fmt_short().to_string().into(),
                    lan: p.lan,
                    ble: p.ble,
                    connected: p.connected,
                    saved: p.saved,
                })
                .collect();
            let state = state.clone();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_peers(ModelRc::new(VecModel::from(items)));
                let mut st = state.lock().unwrap();
                st.peers = peers;
                // Leave the chat if its peer went away.
                if st.selected.is_some_and(|id| !st.peers.iter().any(|p| p.id == id && p.connected)) {
                    st.selected = None;
                }
                show_chat(&ui, &st);
            });
        }
        Event::Groups(view) => {
            let state = state.clone();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                let items: Vec<GroupItem> = view
                    .groups
                    .iter()
                    .map(|g| GroupItem {
                        id: g.id.clone().into(),
                        title: group_title(g).into(),
                        subtitle: if g.alias.is_some() { g.haiku.clone().into() } else { SharedString::default() },
                        summary: group_summary(g).into(),
                    })
                    .collect();
                let invites: Vec<InviteItem> = view
                    .invites
                    .iter()
                    .map(|i| InviteItem {
                        inviter_id: i.inviter.to_string().into(),
                        group_id: i.group.clone().into(),
                        inviter_name: i.inviter_name.clone().into(),
                        group_title: i.haiku.clone().into(),
                    })
                    .collect();
                ui.set_groups(ModelRc::new(VecModel::from(items)));
                ui.set_invites(ModelRc::new(VecModel::from(invites)));
                let mut st = state.lock().unwrap();
                st.groups = view;
                if st.open_group.as_ref().is_some_and(|id| !st.groups.groups.iter().any(|g| g.id == *id)) {
                    st.open_group = None;
                }
                show_group(&ui, &st);
            });
        }
        Event::Message { peer, mine, text } => {
            let state = state.clone();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                let mut st = state.lock().unwrap();
                st.chats.entry(peer).or_default().push(ChatLine { mine, text: text.into() });
                show_chat(&ui, &st);
            });
        }
    }
}

fn show_chat(ui: &MainWindow, st: &UiState) {
    match st.selected {
        Some(id) => {
            let name = st.peers.iter().find(|p| p.id == id).map(|p| p.alias.clone().unwrap_or_else(|| p.name.clone())).unwrap_or_default();
            ui.set_chat_peer_name(name.into());
            ui.set_chat(ModelRc::new(VecModel::from(st.chats.get(&id).cloned().unwrap_or_default())));
            ui.set_chat_peer_id(SharedString::from(id.to_string()));
        }
        None => ui.set_chat_peer_id("".into()),
    }
}

fn group_title(g: &GroupInfo) -> String {
    g.alias.clone().unwrap_or_else(|| g.haiku.clone())
}

fn group_summary(g: &GroupInfo) -> String {
    let count = |s: MemberState| g.members.iter().filter(|m| m.state == s).count();
    let mut parts = vec![format!("{} joined", count(MemberState::Accepted))];
    for (n, label) in [(count(MemberState::Sent), "invited"), (count(MemberState::Pending), "waiting"), (count(MemberState::Declined), "declined")] {
        if n > 0 {
            parts.push(format!("{n} {label}"));
        }
    }
    parts.join(" · ")
}

/// Fill the group detail view (or clear it when no group is open).
fn show_group(ui: &MainWindow, st: &UiState) {
    let group = st.open_group.as_ref().and_then(|id| st.groups.groups.iter().find(|g| g.id == *id));
    let Some(g) = group else {
        ui.set_open_group("".into());
        return;
    };
    let members: Vec<MemberItem> = g
        .members
        .iter()
        .map(|m| {
            let (status, kind) = match m.state {
                MemberState::Accepted => ("joined", 0),
                MemberState::Sent => ("invited", 1),
                MemberState::Pending => ("waiting for them to be online", 2),
                MemberState::Declined => ("declined", 3),
            };
            MemberItem { name: m.name.clone().into(), status: status.into(), kind }
        })
        .collect();
    let candidates: Vec<CandidateItem> =
        g.candidates.iter().map(|(id, name)| CandidateItem { id: id.to_string().into(), name: name.clone().into() }).collect();
    ui.set_group_title(group_title(g).into());
    ui.set_group_haiku(g.haiku.clone().into());
    ui.set_group_alias(g.alias.clone().unwrap_or_default().into());
    ui.set_members(ModelRc::new(VecModel::from(members)));
    ui.set_candidates(ModelRc::new(VecModel::from(candidates)));
    ui.set_open_group(g.id.clone().into());
}
