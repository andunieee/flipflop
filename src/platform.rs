//! Thin layer over the Android activity (JNI). On desktop there is nothing to do here.

#[cfg(target_os = "android")]
pub use android::*;

#[cfg(target_os = "android")]
mod android {
    use std::{
        sync::{Mutex, OnceLock},
        time::{Duration, Instant},
    };

    use jni::{
        JNIEnv, JavaVM,
        objects::{JClass, JObject, JString, JValue},
    };

    /// Read an Android system property.
    pub fn system_property(name: &str) -> Option<String> {
        use std::ffi::{CStr, CString, c_char, c_int};
        unsafe extern "C" {
            fn __system_property_get(name: *const c_char, value: *mut c_char) -> c_int;
        }
        let name = CString::new(name).ok()?;
        let mut buf = [0 as c_char; 92]; // PROP_VALUE_MAX
        let len = unsafe { __system_property_get(name.as_ptr(), buf.as_mut_ptr()) };
        (len > 0).then(|| {
            unsafe { CStr::from_ptr(buf.as_ptr()) }
                .to_string_lossy()
                .into_owned()
        })
    }

    /// The latest Activity: the process can outlive one while files are moving (see `TransferService`).
    static ACTIVITY: Mutex<Option<jni::objects::GlobalRef>> = Mutex::new(None);
    static VM: OnceLock<JavaVM> = OnceLock::new();

    /// Called by every Activity before anything else: hand it to us. The first one also sets up
    /// logging. (`ndk_context`, which blew and friends read, is set up by android-activity.)
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_flipflop_app_FlipflopActivity_nativeInit(
        env: JNIEnv,
        _: JClass,
        activity: JObject,
    ) {
        if let Ok(vm) = env.get_java_vm() {
            let _ = VM.set(vm);
        }
        if let Ok(app) = env.new_global_ref(&activity) {
            *ACTIVITY.lock().unwrap() = Some(app);
        }
        // Quiet by default; `adb shell setprop debug.flipflop.log "warn,iroh_ble_transport=debug"` turns up
        // individual crates (takes effect on the next launch).
        let filter = system_property("debug.flipflop.log").unwrap_or_else(|| "warn".into());
        android_logger::init_once(
            android_logger::Config::default()
                .with_max_level(log::LevelFilter::Trace)
                .with_filter(android_logger::FilterBuilder::new().parse(&filter).build())
                .with_tag("flipflop"),
        );
    }

    /// Run `f` with an attached JNIEnv and the Activity object.
    pub fn with_activity<R>(
        f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<R>,
    ) -> Option<R> {
        let activity = ACTIVITY.lock().unwrap().clone()?;
        let mut env = VM.get()?.attach_current_thread().ok()?;
        let result = f(&mut env, activity.as_obj());
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_describe();
            let _ = env.exception_clear();
        }
        result.map_err(|e| log::error!("jni call failed: {e}")).ok()
    }

    pub fn call_void(method: &str) {
        with_activity(|env, a| env.call_method(a, method, "()V", &[]).map(|_| ()));
    }

    pub fn call_string(method: &str, arg: &str) {
        with_activity(|env, a| {
            let s = env.new_string(arg)?;
            env.call_method(a, method, "(Ljava/lang/String;)V", &[JValue::Object(&s)])
                .map(|_| ())
        });
    }

    fn ble_permissions_granted() -> bool {
        with_activity(|env, a| env.call_method(a, "blePermissionsGranted", "()Z", &[])?.z())
            .unwrap_or(false)
    }

    /// Hand blew (the BLE layer under iroh-ble-transport) the JVM, then make sure the Bluetooth
    /// permission is granted, asking if needed. Returns false if it was denied or timed out.
    pub fn ensure_ble_permission() -> bool {
        let Some(vm) = VM.get() else {
            return false;
        };
        blew::platform::android::init_jvm(unsafe { jni22::JavaVM::from_raw(vm.get_java_vm_pointer().cast()) });
        if ble_permissions_granted() {
            return true;
        }
        call_void("requestBlePermissions");
        for _ in 0..600 {
            std::thread::sleep(std::time::Duration::from_millis(200));
            if ble_permissions_granted() {
                return true;
            }
        }
        false
    }

    /// Keep the app running, with a notification, while something is moving; let it go after. The
    /// count changing shows at once, mere progress once a second (Android drops faster updates).
    pub fn show_transfers(now: &crate::core::TransfersNow) {
        type Shown = (Instant, usize, usize, usize, usize);
        static SHOWN: Mutex<Option<Shown>> = Mutex::new(None);
        let jobs = (now.receiving, now.sending, now.preparing, now.queued);
        {
            let mut shown = SHOWN.lock().unwrap();
            if let Some((at, r, s, p, q)) = *shown
                && (r, s, p, q) == jobs
                && at.elapsed() < Duration::from_secs(1)
            {
                return;
            }
            *shown = Some((Instant::now(), jobs.0, jobs.1, jobs.2, jobs.3));
        }
        let (title, text, percent) = now.describe();
        with_activity(|env, a| {
            let (title, text) = (env.new_string(title)?, env.new_string(text)?);
            env.call_method(
                a,
                "transfersChanged",
                "(Ljava/lang/String;Ljava/lang/String;I)V",
                &[
                    JValue::Object(&title),
                    JValue::Object(&text),
                    JValue::Int(percent),
                ],
            )
            .map(|_| ())
        });
    }

    /// Move a downloaded file or folder into the public Downloads folder; returns where it ended up.
    pub fn file_received(path: &std::path::Path) -> std::path::PathBuf {
        let moved = with_activity(|env, a| {
            let arg = env.new_string(path.to_string_lossy())?;
            let obj = env
                .call_method(
                    a,
                    "saveToDownloads",
                    "(Ljava/lang/String;)Ljava/lang/String;",
                    &[JValue::Object(&arg)],
                )?
                .l()?;
            Ok(jstr(env, &JString::from(obj)))
        })
        .unwrap_or_default();
        if moved.is_empty() {
            path.to_owned()
        } else {
            moved.into()
        }
    }

    pub fn copy_text(text: &str) {
        call_string("copyText", text);
    }

    pub fn paste_text() -> String {
        with_activity(|env, a| {
            let obj = env
                .call_method(a, "pasteText", "()Ljava/lang/String;", &[])?
                .l()?;
            Ok(jstr(env, &JString::from(obj)))
        })
        .unwrap_or_default()
    }

    pub fn open_url(url: &str) {
        call_string("openUrl", url);
    }

    /// Open a downloaded file (in the public Downloads folder) with an app that can show it.
    pub fn open_file(path: &str) {
        call_string("openFile", path);
    }

    /// Ask the Activity to show a system picker; `kind` is "file" or "media". The chosen files arrive
    /// as [`ActivityEvent::FilePicked`].
    pub fn pick_files(kind: &str) {
        call_string("pickFiles", kind);
    }

    /// `(path of the base APK, label, detail)` for every launchable app.
    pub fn list_apps() -> Vec<(String, String, String)> {
        let json = with_activity(|env, a| {
            let obj = env
                .call_method(a, "listApps", "()Ljava/lang/String;", &[])?
                .l()?;
            Ok(jstr(env, &JString::from(obj)))
        })
        .unwrap_or_default();
        serde_json::from_str::<Vec<(String, String, String)>>(&json).unwrap_or_default()
    }

    /// What the Activity tells the app.
    pub enum ActivityEvent {
        /// The user picked a file, or another app shared one, for `group` ("" = the open one). `path`
        /// is a `content://` URI (shared where it is; `name` is its display name) or an app-storage
        /// copy (`name` empty).
        FilePicked {
            kind: String,
            path: String,
            name: String,
            group: String,
        },
        /// Another app shared a text and/or `files` files: the user picks where they go, then
        /// [`deliver_shared`] stages the files.
        Shared { text: String, files: u32 },
        QrScanned(String),
        /// Room taken by the system bars and the keyboard, in logical pixels, and whether the
        /// keyboard is up.
        Insets {
            left: f32,
            top: f32,
            right: f32,
            bottom: f32,
            keyboard: bool,
        },
    }

    type Handler = std::sync::Arc<dyn Fn(ActivityEvent) + Send + Sync>;
    /// The handler, or what the Activity reported before there was one (a share that started the app).
    static HANDLER: Mutex<(Option<Handler>, Vec<ActivityEvent>)> = Mutex::new((None, Vec::new()));

    /// Called on the Java main thread with everything the Activity reports, starting with what it
    /// reported before.
    pub fn set_handler(handler: impl Fn(ActivityEvent) + Send + Sync + 'static) {
        let handler: Handler = std::sync::Arc::new(handler);
        let pending = {
            let mut slot = HANDLER.lock().unwrap();
            slot.0 = Some(handler.clone());
            std::mem::take(&mut slot.1)
        };
        for event in pending {
            handler(event);
        }
    }

    fn report(event: ActivityEvent) {
        let handler = {
            let mut slot = HANDLER.lock().unwrap();
            match &slot.0 {
                Some(handler) => handler.clone(),
                None => return slot.1.push(event),
            }
        };
        handler(event);
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_flipflop_app_FlipflopActivity_nativeFilePicked(
        mut env: JNIEnv,
        _: JClass,
        kind: JString,
        path: JString,
        name: JString,
        group: JString,
    ) {
        report(ActivityEvent::FilePicked {
            kind: jstr(&mut env, &kind),
            path: jstr(&mut env, &path),
            name: jstr(&mut env, &name),
            group: jstr(&mut env, &group),
        });
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_flipflop_app_FlipflopActivity_nativeShared(
        mut env: JNIEnv,
        _: JClass,
        text: JString,
        files: jni::sys::jint,
    ) {
        let text = jstr(&mut env, &text);
        report(ActivityEvent::Shared {
            text,
            files: files.max(0) as u32,
        });
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_flipflop_app_FlipflopActivity_nativeInsets(
        _: JNIEnv,
        _: JClass,
        left: jni::sys::jfloat,
        top: jni::sys::jfloat,
        right: jni::sys::jfloat,
        bottom: jni::sys::jfloat,
        keyboard: jni::sys::jboolean,
    ) {
        report(ActivityEvent::Insets {
            left,
            top,
            right,
            bottom,
            keyboard: keyboard != 0,
        });
    }

    /// Stage the files of the last share for `group`; each arrives as [`ActivityEvent::FilePicked`].
    pub fn deliver_shared(group: &str) {
        call_string("deliverShared", group);
    }

    /// Dark system bar icons (on a light UI) or light ones.
    pub fn set_light_bars(light: bool) {
        with_activity(|env, a| {
            env.call_method(a, "setLightBars", "(Z)V", &[JValue::Bool(light.into())])
                .map(|_| ())
        });
    }

    /// Nothing left to go back to: leave the app the way Back leaves any other.
    pub fn move_to_back() {
        with_activity(|env, a| {
            env.call_method(a, "moveTaskToBack", "(Z)Z", &[JValue::Bool(1)])
                .map(|_| ())
        });
    }

    /// A new read-only descriptor for a picked `content://` file (the caller owns it).
    pub fn open_content_fd(uri: &str) -> Option<std::os::fd::OwnedFd> {
        use std::os::fd::FromRawFd;
        let fd = with_activity(|env, a| {
            let arg = env.new_string(uri)?;
            env.call_method(
                a,
                "openContentFd",
                "(Ljava/lang/String;)I",
                &[JValue::Object(&arg)],
            )?
            .i()
        })?;
        (fd >= 0).then(|| unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
    }

    /// We stopped sharing this `content://` file: give its read permission back.
    pub fn release_content_uri(uri: &str) {
        call_string("releaseContentUri", uri);
    }

    pub fn start_qr_scan() {
        call_void("startQrScan");
    }

    fn jstr(env: &mut JNIEnv, s: &JString) -> String {
        env.get_string(s).map(Into::into).unwrap_or_default()
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_flipflop_app_FlipflopActivity_nativeQrScanned(
        mut env: JNIEnv,
        _: JClass,
        text: JString,
    ) {
        report(ActivityEvent::QrScanned(jstr(&mut env, &text)));
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_flipflop_app_FlipflopActivity_nativeBlePeer(
        mut env: JNIEnv,
        _: JClass,
        address: JString,
        json: JString,
    ) {
        let (address, json) = (jstr(&mut env, &address), jstr(&mut env, &json));
        crate::ble::android::on_peer(address, json);
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_flipflop_app_FlipflopActivity_nativeBleLost(
        mut env: JNIEnv,
        _: JClass,
        address: JString,
    ) {
        let address = jstr(&mut env, &address);
        crate::ble::android::on_lost(address);
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_flipflop_app_FlipflopActivity_nativeBleStatus(
        mut env: JNIEnv,
        _: JClass,
        text: JString,
    ) {
        let text = jstr(&mut env, &text);
        crate::ble::android::on_status(text);
    }
}

/// Whether the BLE transport can be started now. On Android this asks for the Bluetooth permission
/// and waits for the answer (the transport can only be added when the endpoint is bound);
/// elsewhere there is nothing to ask.
#[cfg(not(target_os = "android"))]
pub fn ensure_ble_permission() -> bool {
    true
}

/// Only Android stops apps that are out of sight, so only there is anything to do.
#[cfg(not(target_os = "android"))]
pub fn show_transfers(_: &crate::core::TransfersNow) {}

/// A downloaded file or folder is complete. On Android it is moved into the public Downloads folder; the
/// returned path is where it ended up.
#[cfg(not(target_os = "android"))]
pub fn file_received(path: &std::path::Path) -> std::path::PathBuf {
    path.to_owned()
}

#[cfg(not(target_os = "android"))]
mod desktop {
    use std::sync::Mutex;

    // The clipboard must outlive the copy on Linux, or the text disappears with it.
    static CLIPBOARD: Mutex<Option<arboard::Clipboard>> = Mutex::new(None);

    fn with_clipboard<R>(f: impl FnOnce(&mut arboard::Clipboard) -> Option<R>) -> Option<R> {
        let mut guard = CLIPBOARD.lock().ok()?;
        if guard.is_none() {
            *guard = arboard::Clipboard::new()
                .map_err(|e| tracing::warn!("no clipboard: {e}"))
                .ok();
        }
        f(guard.as_mut()?)
    }

    pub fn copy_text(text: &str) {
        with_clipboard(|c| c.set_text(text.to_owned()).ok());
    }

    pub fn paste_text() -> String {
        with_clipboard(|c| c.get_text().ok()).unwrap_or_default()
    }

    /// Select-to-copy: put `text` in the PRIMARY selection, which middle-click pastes. Linux only.
    pub fn copy_primary(text: &str) {
        #[cfg(target_os = "linux")]
        with_clipboard(|c| {
            use arboard::{LinuxClipboardKind, SetExtLinux};
            c.set()
                .clipboard(LinuxClipboardKind::Primary)
                .text(text.to_owned())
                .ok()
        });
        #[cfg(not(target_os = "linux"))]
        let _ = text;
    }

    /// The PRIMARY selection (what middle-click pastes); empty off Linux.
    pub fn paste_primary() -> String {
        #[cfg(target_os = "linux")]
        return with_clipboard(|c| {
            use arboard::{GetExtLinux, LinuxClipboardKind};
            c.get().clipboard(LinuxClipboardKind::Primary).text().ok()
        })
        .unwrap_or_default();
        #[cfg(not(target_os = "linux"))]
        String::new()
    }

    pub fn open_url(url: &str) {
        if let Err(err) = open::that_detached(url) {
            tracing::warn!("could not open {url}: {err}");
        }
    }

    pub fn open_file(path: &str) {
        open_url(path);
    }
}

#[cfg(not(target_os = "android"))]
pub use desktop::*;
