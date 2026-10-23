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

use crate::core::{Core, Event, Peer};

slint::include_modules!();

#[derive(Default)]
struct UiState {
    chats: HashMap<EndpointId, Vec<ChatLine>>,
    peers: Vec<Peer>,
    selected: Option<EndpointId>,
}

type State = Arc<Mutex<UiState>>;

pub fn run(data_dir: PathBuf) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let handle = rt.handle().clone();
    let _guard = rt.enter();

    let ui = MainWindow::new()?;
    ui.set_live_camera(cfg!(not(target_os = "android")));
    let state: State = Arc::default();

    let core = {
        let (weak, state) = (ui.as_weak(), state.clone());
        rt.block_on(Core::start(&data_dir, move |event| on_event(&weak, &state, event)))?
    };

    {
        let weak = ui.as_weak();
        crate::ble::start(core.clone(), move |text| {
            let _ = weak.upgrade_in_event_loop(move |ui| ui.set_ble_status(text.into()));
        });
    }

    #[cfg(target_os = "android")]
    {
        let (core, weak, handle) = (core.clone(), ui.as_weak(), handle.clone());
        crate::platform::set_qr_handler(move |text| connect_ticket(&core, &weak, &handle, text));
    }

    ui.on_rename({
        let core = core.clone();
        move |name| core.set_name(&name)
    });

    ui.on_connect({
        let (core, weak, handle) = (core.clone(), ui.as_weak(), handle.clone());
        move |id| {
            let Ok(id) = id.parse::<EndpointId>() else { return };
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

    ui.on_disconnect({
        let core = core.clone();
        move |id| {
            if let Ok(id) = id.parse::<EndpointId>() {
                core.disconnect(id);
            }
        }
    });

    ui.on_connect_ticket({
        let (core, weak, handle) = (core.clone(), ui.as_weak(), handle.clone());
        move |text| connect_ticket(&core, &weak, &handle, text.to_string())
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
        let (core, weak, state, handle) = (core.clone(), ui.as_weak(), state.clone(), handle.clone());
        move |text| {
            let text = text.trim().to_string();
            let Some(id) = state.lock().unwrap().selected else { return };
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
            let (core, weak, handle, scanner) = (core.clone(), ui.as_weak(), handle.clone(), scanner.clone());
            move || {
                let (frame_weak, done_weak) = (weak.clone(), weak.clone());
                let (core, handle) = (core.clone(), handle.clone());
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
    rt.block_on(core.shutdown());
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
                    name: p.name.clone().into(),
                    lan: p.lan,
                    ble: p.ble,
                    connected: p.connected,
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
            let name = st.peers.iter().find(|p| p.id == id).map(|p| p.name.clone()).unwrap_or_default();
            ui.set_chat_peer_name(name.into());
            ui.set_chat(ModelRc::new(VecModel::from(st.chats.get(&id).cloned().unwrap_or_default())));
            ui.set_chat_peer_id(SharedString::from(id.to_string()));
        }
        None => ui.set_chat_peer_id("".into()),
    }
}
