//! Thin layer over the Android activity (JNI). On desktop there is nothing to do here.

#[cfg(target_os = "android")]
pub use android::*;

#[cfg(target_os = "android")]
mod android {
    use std::sync::OnceLock;

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
        (len > 0).then(|| unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned())
    }

    struct ActivityRef(jni::objects::GlobalRef);
    static ACTIVITY: OnceLock<ActivityRef> = OnceLock::new();

    /// Remember the Activity (`AndroidApp::activity_as_ptr`); `ndk_context` only has the Application.
    pub fn init(activity: *mut std::ffi::c_void) {
        let ctx = ndk_context::android_context();
        let Ok(vm) = (unsafe { JavaVM::from_raw(ctx.vm().cast()) }) else { return };
        let Ok(env) = vm.attach_current_thread() else { return };
        let activity = unsafe { JObject::from_raw(activity.cast()) };
        if let Ok(global) = env.new_global_ref(&activity) {
            let _ = ACTIVITY.set(ActivityRef(global));
        }
    }

    /// Run `f` with an attached JNIEnv and the Activity object.
    pub fn with_activity<R>(f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<R>) -> Option<R> {
        let activity = ACTIVITY.get()?;
        let ctx = ndk_context::android_context();
        let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.ok()?;
        let mut env = vm.attach_current_thread().ok()?;
        let result = f(&mut env, activity.0.as_obj());
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
            env.call_method(a, method, "(Ljava/lang/String;)V", &[JValue::Object(&s)]).map(|_| ())
        });
    }

    fn ble_permissions_granted() -> bool {
        with_activity(|env, a| env.call_method(a, "blePermissionsGranted", "()Z", &[])?.z()).unwrap_or(false)
    }

    /// Hand blew (the BLE layer under iroh-ble-transport) the JVM, then make sure the Bluetooth
    /// permission is granted, asking if needed. Returns false if it was denied or timed out.
    pub fn ensure_ble_permission() -> bool {
        let ctx = ndk_context::android_context();
        blew::platform::android::init_jvm(unsafe { jni22::JavaVM::from_raw(ctx.vm().cast()) });
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

    /// Move a downloaded file into the public Downloads folder; returns where it ended up.
    pub fn file_received(path: &std::path::Path) -> std::path::PathBuf {
        let moved = with_activity(|env, a| {
            let arg = env.new_string(path.to_string_lossy())?;
            let obj = env.call_method(a, "saveToDownloads", "(Ljava/lang/String;)Ljava/lang/String;", &[JValue::Object(&arg)])?.l()?;
            Ok(jstr(env, &JString::from(obj)))
        })
        .unwrap_or_default();
        if moved.is_empty() { path.to_owned() } else { moved.into() }
    }

    pub fn copy_text(text: &str) {
        call_string("copyText", text);
    }

    pub fn paste_text() -> String {
        with_activity(|env, a| {
            let obj = env.call_method(a, "pasteText", "()Ljava/lang/String;", &[])?.l()?;
            Ok(jstr(env, &JString::from(obj)))
        })
        .unwrap_or_default()
    }

    pub fn open_url(url: &str) {
        call_string("openUrl", url);
    }

    /// Ask the Activity to show a system picker; `kind` is "file" or "media". The chosen files are
    /// copied into app storage and arrive through [`set_file_handler`].
    pub fn pick_files(kind: &str) {
        call_string("pickFiles", kind);
    }

    /// `(path of the base APK, label, detail)` for every launchable app.
    pub fn list_apps() -> Vec<(String, String, String)> {
        let json = with_activity(|env, a| {
            let obj = env.call_method(a, "listApps", "()Ljava/lang/String;", &[])?.l()?;
            Ok(jstr(env, &JString::from(obj)))
        })
        .unwrap_or_default();
        serde_json::from_str::<Vec<(String, String, String)>>(&json).unwrap_or_default()
    }

    type FileHandler = Box<dyn Fn(String, String) + Send + Sync>;
    static FILE_HANDLER: OnceLock<FileHandler> = OnceLock::new();

    /// Called with `(kind, path)` for each file the user picked.
    pub fn set_file_handler(handler: impl Fn(String, String) + Send + Sync + 'static) {
        let _ = FILE_HANDLER.set(Box::new(handler));
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_flipflop_app_FlipflopActivity_nativeFilePicked(
        mut env: JNIEnv,
        _: JClass,
        kind: JString,
        path: JString,
    ) {
        let (kind, path) = (jstr(&mut env, &kind), jstr(&mut env, &path));
        if let Some(handler) = FILE_HANDLER.get() {
            handler(kind, path);
        }
    }

    pub fn start_qr_scan() {
        call_void("startQrScan");
    }

    type QrHandler = Box<dyn Fn(String) + Send + Sync>;
    static QR_HANDLER: OnceLock<QrHandler> = OnceLock::new();

    pub fn set_qr_handler(handler: impl Fn(String) + Send + Sync + 'static) {
        let _ = QR_HANDLER.set(Box::new(handler));
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
        let text = jstr(&mut env, &text);
        if let Some(handler) = QR_HANDLER.get() {
            handler(text);
        }
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

/// A downloaded file is complete. On Android it is moved into the public Downloads folder; the
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
            *guard = arboard::Clipboard::new().map_err(|e| tracing::warn!("no clipboard: {e}")).ok();
        }
        f(guard.as_mut()?)
    }

    pub fn copy_text(text: &str) {
        with_clipboard(|c| c.set_text(text.to_owned()).ok());
    }

    pub fn paste_text() -> String {
        with_clipboard(|c| c.get_text().ok()).unwrap_or_default()
    }

    pub fn open_url(url: &str) {
        if let Err(err) = open::that_detached(url) {
            tracing::warn!("could not open {url}: {err}");
        }
    }
}

#[cfg(not(target_os = "android"))]
pub use desktop::*;
