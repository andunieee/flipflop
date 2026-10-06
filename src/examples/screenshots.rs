//! Renders every page with sample data to PPM images, headless (Slint's
//! software renderer; no window, no node, no network). For reviewing UI
//! changes:
//!
//!     cargo run --example screenshots -- <out-dir>
//!
//! Convert with e.g. `magick out/peer.ppm out/peer.png`.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, ModelRc, PhysicalSize, VecModel};
use tunnelmanager_slint::{AppWindow, HistoryRow, OutboxRow, PeerRow, State, TransferRow};

struct Headless(Rc<MinimalSoftwareWindow>);

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

fn model<T: Clone + 'static>(rows: Vec<T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(rows)))
}

fn peer(id: &str, name: &str, initials: &str, online: bool) -> PeerRow {
    PeerRow {
        endpoint_id: id.into(),
        name: name.into(),
        initials: initials.into(),
        detail: "desktop · linux · 3f9a12c0".into(),
        online,
        ..Default::default()
    }
}

fn history(title: &str, send: bool, status: &str, tone: &str, size: &str) -> HistoryRow {
    HistoryRow {
        id: title.into(),
        title: title.into(),
        direction: if send { "send" } else { "receive" }.into(),
        status: status.into(),
        tone: tone.into(),
        detail: if send {
            ""
        } else {
            "/home/me/Downloads/tunnelmanager/Studio Mac"
        }
        .into(),
        date: "2026-10-03 14:12".into(),
        size: size.into(),
        speed: "12.4 MB/s".into(),
        can_open: !send,
        preview: "".into(),
    }
}

fn populate(state: &State<'_>) {
    let mut peers = vec![
        peer("a", "Studio Mac", "SM", true),
        peer("b", "Pixel 8", "P", true),
        peer("c", "Living Room TV", "LR", false),
        peer("d", "work-laptop", "W", false),
    ];
    peers[0].sending = true;
    peers[1].receiving = true;
    state.set_peers(model(peers));
    state.set_presence_label("2 of 4 online".into());
    state.set_my_name("andunie's desktop".into());
    state.set_name_input("andunie's desktop".into());
    state.set_node_ready(true);
    state.set_selected_id("a".into());
    state.set_selected_name("Studio Mac".into());
    state.set_selected_initials("SM".into());
    state.set_selected_detail("desktop · macos · 3f9a12c0".into());
    state.set_selected_online(true);
    state.set_selected_sending(true);

    let sending = TransferRow {
        key: "send-1".into(),
        peer_id: "a".into(),
        sending: true,
        active: true,
        title: "holiday-photos".into(),
        progress: 0.42,
        progress_label: "42% · 1.2 GB of 2.9 GB".into(),
        speed: "38.5 MB/s".into(),
        status: "Transferring…".into(),
        ..Default::default()
    };
    let pasted = TransferRow {
        key: "recv-3".into(),
        peer_id: "a".into(),
        sending: false,
        active: false,
        title: "text".into(),
        progress: 1.0,
        status: "Saved".into(),
        text: "https://example.com/articles/how-tunnels-work".into(),
        is_link: true,
        ..Default::default()
    };
    let received = TransferRow {
        key: "recv-2".into(),
        peer_id: "a".into(),
        sending: false,
        active: false,
        title: "3 items".into(),
        progress: 1.0,
        progress_label: "100%".into(),
        status: "Saved to /home/me/Downloads/tunnelmanager/Studio Mac".into(),
        note: "2 files renamed to avoid overwriting".into(),
        ..Default::default()
    };
    state.set_transfers(model(vec![
        sending.clone(),
        pasted.clone(),
        received.clone(),
    ]));
    state.set_visible_transfers(model(vec![sending, pasted, received]));
    state.set_paste_input("Door code is 4711 — see you at 8!".into());

    state.set_history(model(vec![
        history("quarterly-report.pdf", true, "Completed", "ok", "2.4 MB"),
        HistoryRow {
            title: "Pasted text".into(),
            detail: "tunnelmanager-paste.txt".into(),
            preview: "Door code is 4471, the spare key is under the blue pot by the back steps — feed the cat twice a day".into(),
            ..history("paste", false, "Completed", "ok", "96 B")
        },
        history("holiday-photos", false, "Interrupted", "warn", "2.9 GB"),
        history("project-archive.tar.zst", true, "Failed", "error", "820 MB"),
        history("notes.md", false, "Cancelled", "muted", "4.1 KB"),
        history("song.flac", false, "Completed", "ok", "38.0 MB"),
    ]));

    state.set_suggestions(model(vec![
        PeerRow {
            endpoint_id: "r".into(),
            name: "Kitchen iPad".into(),
            initials: "KI".into(),
            detail: "wants to pair with you".into(),
            is_request: true,
            online: true,
            ..Default::default()
        },
        PeerRow {
            endpoint_id: "n".into(),
            name: "raspberrypi".into(),
            initials: "R".into(),
            detail: "Found on your local network".into(),
            is_suggestion: true,
            online: true,
            ..Default::default()
        },
    ]));
    state.set_my_ticket(
        "pairab3xkq7lmz2vcd9u4ohw6gtnr5yfej8si0pq1x3b7m2kz9u5c4wv8hd6tn0ry3jf7".into(),
    );
    state.set_pairing_status("Pair request sent — waiting for them to accept".into());
    state.set_downloads_dir("/home/me/Downloads".into());
    state.set_relay_mode(2);
    state.set_relay_urls("https://relay1.example.com\nhttps://relay2.example.com".into());
    state.set_relay_test_status("Relay check failed: connection refused".into());
    state.set_relay_test_failed(true);
}

fn render(window: &MinimalSoftwareWindow, size: (u32, u32), path: &Path) {
    window.set_size(PhysicalSize::new(size.0, size.1));
    slint::platform::update_timers_and_animations();
    window.request_redraw();
    let mut buffer = vec![PremultipliedRgbaColor::default(); (size.0 * size.1) as usize];
    window.draw_if_needed(|renderer| {
        renderer.render(&mut buffer, size.0 as usize);
    });
    let mut ppm = format!("P6\n{} {}\n255\n", size.0, size.1).into_bytes();
    for px in &buffer {
        ppm.extend_from_slice(&[px.red, px.green, px.blue]);
    }
    std::fs::write(path, ppm).expect("write screenshot");
    println!("wrote {}", path.display());
}

fn main() {
    let out = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "screenshots".into()),
    );
    std::fs::create_dir_all(&out).expect("create output dir");

    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();

    let ui = AppWindow::new().unwrap();
    let state = ui.global::<State>();
    populate(&state);
    ui.show().unwrap();

    let desktop = (1040, 720);
    for page in ["peer", "add-peer", "settings"] {
        state.set_page(page.into());
        render(&window, desktop, &out.join(format!("{page}.ppm")));
    }

    state.set_page("peer".into());
    state.set_toast_text("Files from Studio Mac saved".into());
    render(&window, desktop, &out.join("toast.ppm"));
    state.set_toast_text("".into());

    // No peers yet.
    state.set_peers(model(Vec::new()));
    state.set_selected_name("".into());
    state.set_selected_id("".into());
    state.set_visible_transfers(model(Vec::new()));
    render(&window, desktop, &out.join("empty.ppm"));
    populate(&state);

    // Phone layout, with room for the status and navigation bars.
    state.set_touch(true);
    state.set_compact(true);
    ui.set_preview_inset_top(24.0);
    ui.set_preview_inset_bottom(48.0);
    let phone = (412, 860);
    state.set_peer_open(true);
    for page in ["peer", "add-peer", "settings"] {
        state.set_page(page.into());
        render(&window, phone, &out.join(format!("phone-{page}.ppm")));
    }

    // Phone: the peer list (Peers tab top level), then a peer's own screen,
    // both with files shared in from another app.
    state.set_page("peer".into());
    state.set_selected_sending(false);
    let outbox = vec![
        OutboxRow {
            path: "/outbox/IMG_2041.jpg".into(),
            name: "IMG_2041.jpg".into(),
            size: "3.2 MB".into(),
        },
        OutboxRow {
            path: "/outbox/notes.pdf".into(),
            name: "notes.pdf".into(),
            size: "180 KB".into(),
        },
    ];
    state.set_outbox_count(outbox.len() as i32);
    state.set_outbox(model(outbox));
    state.set_peer_open(false);
    render(&window, phone, &out.join("phone-peers.ppm"));
    state.set_peer_open(true);
    render(&window, phone, &out.join("phone-outbox.ppm"));
    state.set_outbox_count(0);
    state.set_outbox(model(Vec::new()));
    state.set_peer_open(false);

    // No peers yet.
    state.set_peers(model(Vec::new()));
    state.set_selected_name("".into());
    state.set_selected_id("".into());
    render(&window, phone, &out.join("phone-empty.ppm"));
}
