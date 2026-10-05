//! Per-platform services: clipboard, toasts, native dialogs, "open file" and
//! the data directory. JNI on Android (see `android.rs`), rfd/arboard/open
//! on desktop.

use crate::AppWindow;
use std::path::PathBuf;

#[cfg(target_os = "android")]
use crate::android;
#[cfg(not(target_os = "android"))]
use crate::State;
#[cfg(not(target_os = "android"))]
use slint::ComponentHandle;

/// Phone-friendly layout (single pane, bigger hit targets) on touch devices.
pub const TOUCH: bool = cfg!(target_os = "android");

#[cfg(target_os = "android")]
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    android::copy_to_clipboard(text)
}

#[cfg(not(target_os = "android"))]
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
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

/// Short notification. Must be called on the UI thread.
#[cfg(target_os = "android")]
pub fn toast(_ui: &AppWindow, msg: &str, error: bool) {
    // Native toast/snackbar; the UI itself stays untouched.
    android::show_toast(msg, error);
}

/// Short notification. Must be called on the UI thread.
#[cfg(not(target_os = "android"))]
pub fn toast(ui: &AppWindow, msg: &str, error: bool) {
    thread_local! {
        // Restarted by every toast, so the latest message gets the full time.
        static HIDE_TIMER: slint::Timer = slint::Timer::default();
    }

    let state = ui.global::<State>();
    state.set_toast_text(msg.into());
    state.set_toast_error(error);
    let weak = ui.as_weak();
    let visible_for = std::time::Duration::from_secs(if error { 6 } else { 3 });
    HIDE_TIMER.with(|timer| {
        timer.start(slint::TimerMode::SingleShot, visible_for, move || {
            if let Some(ui) = weak.upgrade() {
                ui.global::<State>().set_toast_text("".into());
            }
        });
    });
}

/// Toast from any thread.
pub fn toast_later(weak: &slint::Weak<AppWindow>, msg: impl Into<String>, error: bool) {
    let msg = msg.into();
    let _ = weak.upgrade_in_event_loop(move |ui| toast(&ui, &msg, error));
}

/// Files/folders the user wants to send. Empty = cancelled. Blocks on a
/// native dialog, so call it off the UI thread.
#[cfg(not(target_os = "android"))]
pub fn pick_send_paths() -> Vec<PathBuf> {
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
pub fn pick_downloads_folder() -> Option<PathBuf> {
    // Android: keep the engine default (app-private Downloads dir); the
    // SAF directory picker needs activity-result plumbing that
    // android-activity does not expose.
    None
}

/// Folder to store received files in. `None` keeps the default.
#[cfg(not(target_os = "android"))]
pub fn pick_downloads_folder() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Choose downloads folder")
        .pick_folder()
}

/// Hand a saved file back to the OS (file manager / viewer).
#[cfg(target_os = "android")]
pub fn open_path(_path: &str) {
    // No-op: surfacing received files is done through the system
    // Downloads/Files app on Android.
}

/// Hand a saved file back to the OS (file manager / viewer).
#[cfg(not(target_os = "android"))]
pub fn open_path(path: &str) {
    if let Err(e) = open::that(path) {
        tracing::warn!("failed to open {path:?}: {e}");
    }
}

/// Base directory for settings + history store. On Android this is the
/// app-private data dir (`/data/data/<pkg>/files`); `dirs::data_dir()` has
/// no meaning there.
#[cfg(target_os = "android")]
pub fn default_data_dir() -> PathBuf {
    android::data_dir()
}

#[cfg(not(target_os = "android"))]
pub fn default_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tunnelmanager-slint")
}

/// Log destination: logcat on Android (stdout goes nowhere there), stderr
/// elsewhere.
pub fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    #[cfg(target_os = "android")]
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .without_time()
        .with_writer(android::Logcat)
        .init();
    #[cfg(not(target_os = "android"))]
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
