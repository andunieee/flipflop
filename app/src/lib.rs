//! flipflop's UI (Freya) over the node in the `flipflop` crate: the desktop app and the Android app.

mod backend;
mod group;
mod icons;
mod model;
mod nav;
mod pad;
mod pages;
#[cfg(not(target_os = "android"))]
mod scan;
mod shell;
mod theme;
mod widgets;

use flipflop::core::Event;
use freya::{prelude::*, radio::*};
#[cfg(target_os = "android")]
use freya::android::AndroidExt;

pub use crate::backend::Backend;
use crate::{
    backend::Msg,
    model::{Channel, Model},
    nav::{Nav, Page, Share, Ui},
    theme::{Dim, Look, Palette},
};

/// The whole app, over a running [`Backend`] (provided as root context).
pub fn app() -> impl IntoElement {
    let backend = use_consume::<Backend>();
    let backend_state = use_state(|| backend.clone());
    let ui = use_state(|| Ui::load(&backend));
    let nav = use_provide_context(|| Nav::new(ui, backend_state));
    let station = use_init_radio_station::<Model, Channel>(Model::default);

    let window = Platform::get().current_window();
    let theme = ui.read().theme.clone();
    let dark = match theme.as_str() {
        "light" => false,
        "dark" => true,
        _ => *window.preferred_theme.read() == PreferredTheme::Dark,
    };
    let narrow = window.root_size.read().width < 420.;
    let look = Look {
        palette: Palette::new(dark),
        dim: Dim {
            touch: theme::touch(),
            narrow,
        },
    };
    let mut look_state = use_provide_context(|| State::create(look));
    look_state.set_if_modified(look);
    let mut freya_theme = use_init_theme(|| look.palette.freya_theme());
    use_side_effect(move || {
        let palette = look_state.read().palette;
        if freya_theme.peek().name != palette.freya_theme().name {
            freya_theme.set(palette.freya_theme());
        }
    });

    // Dark status bar icons on the light theme, light ones on the dark.
    #[cfg(target_os = "android")]
    use_side_effect(move || {
        let dark = look_state.read().palette.dark;
        flipflop::platform::set_light_bars(!dark);
    });

    use_hook(move || {
        let mut rx = backend.attach();
        spawn(async move {
            while let Some(msg) = rx.recv().await {
                apply(msg, station, nav);
            }
        });
    });

    shell::Shell
}

/// Whether a text field is being edited, which on Android means the soft keyboard is up for it.
#[cfg(target_os = "android")]
fn editing() -> bool {
    use freya_winit::integration::is_ime_role;
    is_ime_role(Platform::get().current_window().focused_accessibility_node.peek().role())
}

/// Stop editing a text field; returns whether there was one. On Android the keyboard belongs to the
/// focused field: hiding it while the field keeps the focus does not last, because the framework
/// shows it again on the next accessibility update (and hiding it causes one: the layout grows as
/// the keyboard goes). So move the focus away as well.
#[cfg(target_os = "android")]
pub(crate) fn stop_editing() -> bool {
    if !editing() {
        return false;
    }
    // The root node (`ACCESSIBILITY_ROOT_ID` in freya-core) is not a text input, so the keyboard
    // stays down.
    let platform = Platform::get();
    platform.current_window().send(UserEvent::FocusAccessibilityNode(
        AccessibilityFocusStrategy::Node(AccessibilityId(0)),
    ));
    // Hide it now rather than wait for the focus update: the framework only hides keyboards it
    // considers implicit, and this one was shown explicitly.
    if let Err(err) = platform.hide_keyboard() {
        tracing::warn!("keyboard: {err:?}");
    }
    true
}

/// What the node (or the Activity) said, into the model.
fn apply(msg: Msg, mut station: RadioStation<Model, Channel>, mut nav: Nav) {
    match msg {
        Msg::Ready { core, download_dir } => {
            let mut m = station.write_channel(Channel::Me);
            m.core = Some(core);
            m.download_dir = download_dir;
        }
        Msg::BleStatus(text) => station.write_channel(Channel::Me).ble_status = text,
        Msg::Notice(text) => nav.toast(text),
        Msg::ShowPeers => nav.go(Page::Peers),
        Msg::Scanning(on) => station.write_channel(Channel::Me).scanning = on,
        Msg::Shared { text, files } => nav.ui.write().share = Some(Share::Shared { text, files }),
        Msg::Insets { insets, keyboard } => {
            let mut m = station.write_channel(Channel::Me);
            m.insets = insets;
            let was_open = std::mem::replace(&mut m.keyboard_open, keyboard);
            drop(m);
            // The user put the keyboard away while a field still had the focus: stop editing, or
            // the next accessibility update brings the keyboard back.
            #[cfg(target_os = "android")]
            if was_open && !keyboard {
                let _ = stop_editing();
            }
            #[cfg(not(target_os = "android"))]
            let _ = was_open;
        }
        Msg::Core(event) => match event {
            Event::Name(name) => station.write_channel(Channel::Me).name = name,
            Event::Ticket(ticket) => station.write_channel(Channel::Me).ticket = ticket,
            Event::Notice(text) => nav.toast(text),
            Event::Peers(peers) => station.write_channel(Channel::Peers).peers = peers,
            Event::Home(news) => station.write_channel(Channel::Home).home = news,
            Event::Transfers(now) => station.write_channel(Channel::Home).transfers = now.items,
            Event::Groups(view) => {
                {
                    let mut m = station.write_channel(Channel::Groups);
                    let ids: Vec<String> = view.groups.iter().map(|g| g.id.clone()).collect();
                    m.views.retain(|id, _| ids.contains(id));
                    m.groups = view;
                }
                // A group that went away takes its page with it.
                let open = nav.ui.peek().open_group().map(str::to_owned);
                if let Some(id) = open
                    && station.peek().group(&id).is_none()
                {
                    nav.close_group();
                }
            }
            Event::Group(view) => {
                // Our own scratchpad edits are not news; they are caught up when the page is left.
                let looking = nav.viewed().as_deref() == Some(view.id.as_str()) && !view.scratch_local;
                let gid = view.id.clone();
                station.write_channel(Channel::Groups).views.insert(gid.clone(), view);
                if looking {
                    nav.backend().mark_seen(vec![gid]);
                }
            }
            Event::Message { .. } => {}
        },
    }
}

/// Start the node in `data_dir` and show the app; returns when the window is closed (desktop).
pub fn launch_app(backend: Backend, config: impl FnOnce(WindowConfig) -> WindowConfig) -> LaunchConfig {
    LaunchConfig::new().with_window(config(WindowConfig::new(app).with_root_context(backend)))
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(droid_app: winit::platform::android::activity::AndroidApp) {
    use std::sync::OnceLock;

    use freya::android::AndroidPlugin;
    use freya_winit::renderer::NativeEvent;
    use winit::{event_loop::EventLoop, platform::android::EventLoopBuilderExtAndroid};

    /// The node: one per process, which can outlive an activity while files are moving.
    static NODE: OnceLock<(tokio::runtime::Runtime, Backend)> = OnceLock::new();
    let (rt, backend) = NODE.get_or_init(|| {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let dir = droid_app
            .internal_data_path()
            .expect("the app's data dir");
        let backend = Backend::start(rt.handle().clone(), dir);
        (rt, backend)
    });
    let _guard = rt.enter();

    let event_loop = EventLoop::<NativeEvent>::with_user_event()
        .with_android_app(droid_app.clone())
        .build()
        .expect("Failed to build event loop");
    launch(
        launch_app(backend.clone(), |w| w)
            .with_plugin(AndroidPlugin::new(droid_app))
            .with_event_loop(event_loop),
    )
}
