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
use slint::{ComponentHandle, Model, ModelRc, PhysicalSize, VecModel};
use flipflop::{AppRow, AppWindow, HistoryRow, OutboxRow, PeerRow, QuickOffer, State, TransferRow};

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
            "/home/me/Downloads/flipflop/my-phone"
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
        peer("a", "my-phone", "MP", true),
        peer("b", "Anna's phone", "AP", true),
        peer("c", "parents", "P", true),
        peer("d", "Work laptop", "WL", false),
    ];
    peers[0].receiving = true;
    peers[0].detail = "phone · android · 3f9a12c0".into();
    state.set_peers(model(peers));
    state.set_presence_label("3 of 4 online".into());
    state.set_my_name("Office desktop".into());
    state.set_name_input("Office desktop".into());
    state.set_node_ready(true);
    state.set_selected_id("a".into());
    state.set_selected_name("my-phone".into());
    state.set_selected_initials("MP".into());
    state.set_selected_detail("phone · android · 3f9a12c0".into());
    state.set_selected_online(true);
    state.set_selected_sending(false);

    let photos = TransferRow {
        key: "recv-1".into(),
        peer_id: "a".into(),
        sending: false,
        active: true,
        title: "18 photos".into(),
        progress: 0.64,
        progress_label: "64% · 54 MB of 84 MB".into(),
        speed: "11.2 MB/s".into(),
        status: "Transferring…".into(),
        ..Default::default()
    };
    state.set_transfers(model(vec![photos.clone()]));
    state.set_visible_transfers(model(vec![photos]));
    state.set_paste_input("Door code is 4711, see you at 8!".into());

    state.set_history(model(vec![
        history("slides-final.pdf", true, "Completed", "ok", "2.4 MB"),
        HistoryRow {
            title: "Pasted text".into(),
            detail: "flipflop-paste.txt".into(),
            preview: "Door code is 4471, the spare key is under the blue pot by the back steps, feed the cat twice a day".into(),
            ..history("paste", false, "Completed", "ok", "96 B")
        },
        history("beach-video.mp4", false, "Interrupted", "warn", "1.1 GB"),
        history("tax-return-2025.zip", true, "Failed", "error", "48 MB"),
        history("shopping-list.txt", false, "Cancelled", "muted", "1.2 KB"),
        history("voice-memo.m4a", false, "Completed", "ok", "6.0 MB"),
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
    let device = |id: &str, name: &str, initials: &str| PeerRow {
        endpoint_id: id.into(),
        name: name.into(),
        initials: initials.into(),
        detail: "On your local network".into(),
        is_suggestion: true,
        online: true,
        ..Default::default()
    };
    state.set_nearby_devices(model(vec![
        device("n", "raspberrypi", "R"),
        device("m", "Marta's laptop", "ML"),
    ]));
    state.set_quick_incoming(model(vec![QuickOffer {
        key: "offer-1".into(),
        sender: "Marta's laptop".into(),
        initials: "ML".into(),
        title: "boarding-pass.pdf".into(),
        size: "412 KB".into(),
    }]));
    state.set_quick_transfers(model(vec![TransferRow {
        key: "send-9".into(),
        peer_id: "n".into(),
        sending: true,
        active: true,
        quick: true,
        title: "slides.pdf to raspberrypi".into(),
        status: "Waiting for raspberrypi to accept…".into(),
        progress_label: "2.4 MB".into(),
        ..Default::default()
    }]));
    state.set_my_ticket(
        "pairab3xkq7lmz2vcd9u4ohw6gtnr5yfej8si0pq1x3b7m2kz9u5c4wv8hd6tn0ry3jf7".into(),
    );
    state.set_pairing_status("Pair request sent, waiting for them to accept".into());
    state.set_downloads_dir("/home/me/Downloads".into());
    state.set_relay_mode(2);
    state.set_relay_urls("https://relay1.example.com\nhttps://relay2.example.com".into());
    state.set_relay_test_status("Relay check failed: connection refused".into());
    state.set_relay_test_failed(true);
}

/// An open browser link with a browser connected, selected.
fn show_guest(state: &State<'_>) {
    let url = "http://192.168.1.5:53530/n3rp36io3ncgwvbr7wzq";
    let qr = flipflop::qr::render(url).expect("qr");
    state.set_guest_url(url.into());
    state.set_guest_qr(slint::Image::from_rgb8(qr));
    state.set_guest_more_addresses(true);
    state.set_guest_upload(false);
    let link = PeerRow {
        endpoint_id: "browser-n3rp36io".into(),
        name: "Browser 1".into(),
        detail: "Safari on iPhone · Same Wi-Fi · download only".into(),
        online: true,
        guest: true,
        ..Default::default()
    };
    let mut peers: Vec<PeerRow> = state.get_peers().iter().collect();
    peers.insert(0, link.clone());
    state.set_peers(model(peers));
    state.set_links(model(vec![link]));
    state.set_selected_id("browser-n3rp36io".into());
    state.set_selected_name("Browser 1".into());
    state.set_selected_initials("".into());
    state.set_selected_detail("Safari on iPhone · Same Wi-Fi · download only".into());
    state.set_selected_guest(true);
    state.set_visible_transfers(model(vec![TransferRow {
        key: "send-1".into(),
        peer_id: "browser-n3rp36io".into(),
        sending: true,
        active: true,
        title: "holiday.mp4".into(),
        progress: 0.4,
        progress_label: "40% · 120 MB of 300 MB".into(),
        speed: "11.2 MB/s".into(),
        status: "Downloading in the browser…".into(),
        ..Default::default()
    }]));
    state.set_history(model(Vec::new()));
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
    for page in ["peer", "add-peer", "settings", "quick-share", "share-link"] {
        state.set_page(page.into());
        render(&window, desktop, &out.join(format!("{page}.ppm")));
    }

    state.set_page("share-link".into());
    state.set_link_receive(true);
    state.set_link_web(true);
    render(&window, desktop, &out.join("share-link-receive.ppm"));
    state.set_link_receive(false);
    state.set_link_web(false);

    state.set_page("peer".into());
    state.set_toast_text("Files from my-phone saved".into());
    render(&window, desktop, &out.join("toast.ppm"));
    state.set_toast_text("".into());

    // The document shared with the peer.
    state.set_peer_tab("doc".into());
    state.set_doc_text("Shopping list\n- milk\n- bread\n\nWi-Fi at the cabin: pinecone42".into());
    render(&window, desktop, &out.join("doc.ppm"));
    state.set_peer_tab("main".into());
    state.set_doc_text("".into());

    show_guest(&state);
    render(&window, desktop, &out.join("guest.ppm"));
    let web_url = "https://andunieee.github.io/flipflop/#t=n3rp36io3ncgwvbr7wzq&e=ae9a9cc0cc827c4e764969d70f936f5ba53be4e2280aa2e31cb878c6fb92c947&r=https%3A%2F%2Fuse1-1.relay.n0.iroh.link.%2F";
    state.set_guest_web(true);
    state.set_guest_upload(true);
    state.set_guest_url(web_url.into());
    state.set_guest_qr(slint::Image::from_rgb8(flipflop::qr::render(web_url).expect("qr")));
    state.set_guest_more_addresses(false);
    render(&window, desktop, &out.join("guest-web.ppm"));
    state.set_guest_web(false);
    show_guest(&state);
    state.set_guest_url("".into());
    state.set_guest_more_addresses(false);
    render(&window, desktop, &out.join("guest-no-network.ppm"));
    state.set_selected_guest(false);
    populate(&state);

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
    for page in ["peer", "add-peer", "settings", "share"] {
        state.set_page(page.into());
        render(&window, phone, &out.join(format!("phone-{page}.ppm")));
    }
    // The whole sharing page, unscrolled.
    render(&window, (412, 2000), &out.join("phone-share-full.ppm"));

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

    // Phone: the shared document.
    state.set_peer_tab("doc".into());
    state.set_doc_text("Shopping list\n- milk\n- bread".into());
    render(&window, phone, &out.join("phone-doc.ppm"));
    state.set_peer_tab("main".into());
    state.set_doc_text("".into());

    // Phone: the "Send app" chooser.
    let apps = [
        ("Firefox", "org.mozilla.firefox", "131.0"),
        ("Signal", "org.thoughtcrime.securesms", "7.20.1 · split APK"),
        ("VLC", "org.videolan.vlc", "3.6.0"),
    ]
    .into_iter()
    .map(|(name, package, detail)| AppRow {
        package: package.into(),
        name: name.into(),
        detail: detail.into(),
        icon: Default::default(),
    })
    .collect();
    state.set_apps(model(apps));
    state.set_app_picker_open(true);
    render(&window, phone, &out.join("phone-apps.ppm"));
    state.set_app_picker_open(false);
    state.set_outbox_count(0);
    state.set_outbox(model(Vec::new()));
    state.set_peer_open(false);

    // Phone: the browser link, in the list and on its own screen.
    show_guest(&state);
    render(&window, phone, &out.join("phone-guest-list.ppm"));
    state.set_peer_open(true);
    render(&window, phone, &out.join("phone-guest.ppm"));
    state.set_peer_open(false);
    state.set_selected_guest(false);
    populate(&state);

    // No peers yet.
    state.set_peers(model(Vec::new()));
    state.set_selected_name("".into());
    state.set_selected_id("".into());
    render(&window, phone, &out.join("phone-empty.ppm"));
}
