//! Android platform services: clipboard, toasts and the shared-content
//! outbox.
//!
//! The UI runs on the `android_main` thread; anything that must touch the
//! JVM (ClipboardManager, Toast, ContentResolver) is bridged with JNI on
//! the Java main thread via `AndroidApp::run_on_java_main_thread`.
//!
//! On non-Android builds this module compiles to no-ops so the rest of the
//! app can reference it unconditionally.

#[cfg(target_os = "android")]
mod imp {
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;

    use jni::objects::{GlobalRef, JObject, JString, JValue};
    use jni::JavaVM;

    use slint::android::android_activity::AndroidApp;

    static ANDROID_APP: OnceLock<AndroidApp> = OnceLock::new();

    /// Store the app handle for later platform calls. Called from
    /// `android_main` before anything else touches this module.
    pub fn set_app(app: AndroidApp) {
        let _ = ANDROID_APP.set(app);
    }

    fn app() -> &'static AndroidApp {
        ANDROID_APP.get().expect("AndroidApp not initialised")
    }

    // ----------------------------------------------------------- data dirs

    /// App-private storage (`internal_data_path` → /data/data/<pkg>/files).
    pub fn data_dir() -> PathBuf {
        let app = app();
        app.internal_data_path()
            .or_else(|| app.external_data_path())
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// Default location for received files.
    pub fn downloads_dir() -> PathBuf {
        data_dir().join("downloads")
    }

    /// Directory where shared-in content is staged before sending.
    pub fn outbox_dir() -> PathBuf {
        data_dir().join("outbox")
    }

    /// Number of files currently staged in the outbox (for the UI hint).
    #[allow(dead_code)]
    pub fn outbox_count() -> usize {
        std::fs::read_dir(outbox_dir())
            .map(|entries| entries.flatten().filter(|e| e.path().is_file()).count())
            .unwrap_or(0)
    }

    /// Remove staged outbox files after they have been shared.
    pub fn clear_outbox(paths: &[PathBuf]) {
        let dir = outbox_dir();
        for path in paths {
            if path.starts_with(&dir) {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    // ---------------------------------------------------------- JNI access

    /// Run `f` with a `JNIEnv` for the current thread.
    fn with_env<T>(f: impl FnOnce(&mut jni::JNIEnv) -> Result<T, String>) -> Result<T, String> {
        // SAFETY: `vm_as_ptr` returns the JavaVM pointer owned by the
        // Android runtime; it stays valid for the process lifetime.
        let vm = unsafe { JavaVM::from_raw(app().vm_as_ptr().cast()) }
            .map_err(|e| format!("JavaVM: {e}"))?;
        let mut guard = vm.attach_current_thread().map_err(|e| e.to_string())?;
        let env = &mut *guard;
        f(env)
    }

    /// Activity object for JNI calls. `AndroidApp` holds a global reference
    /// that lives as long as the app handle; the returned lifetime is
    /// chosen by the caller's JNI scope.
    fn activity<'local>() -> Result<JObject<'local>, String> {
        let raw = app().activity_as_ptr() as jni::sys::jobject;
        if raw.is_null() {
            return Err("activity handle is null".to_string());
        }
        // SAFETY: non-null global reference owned by AndroidApp (kept alive
        // in a process-wide OnceLock); we never wrap or delete it.
        Ok(unsafe { JObject::from_raw(raw) })
    }

    // ------------------------------------------------------ multicast lock

    /// Android drops inbound multicast without a `WifiManager.MulticastLock`.
    /// mDNS publish (outbound) works regardless, but receive needs the lock —
    /// without it a peer sees this device while this device never sees them,
    /// and a pair request the peer sends is never noticed. The `GlobalRef`
    /// keeps the lock object (and thus the held lock) alive for the process
    /// lifetime.
    static MULTICAST_LOCK: OnceLock<GlobalRef> = OnceLock::new();

    /// Marks the launch intent as consumed; stages it exactly once per process.
    static INTENT_STAGED: OnceLock<()> = OnceLock::new();

    pub fn acquire_multicast_lock() {
        match with_env(acquire_multicast_lock_with_env) {
            Ok(lock) => {
                let _ = MULTICAST_LOCK.set(lock);
            }
            Err(e) => {
                tracing::warn!("multicast lock unavailable; mDNS receive may be unreliable: {e}")
            }
        }
    }

    fn acquire_multicast_lock_with_env(env: &mut jni::JNIEnv) -> Result<GlobalRef, String> {
        let activity = activity()?;
        // Context.WIFI_SERVICE == "wifi"
        let service_name = env.new_string("wifi").map_err(|e| e.to_string())?;
        let wifi = env
            .call_method(
                &activity,
                "getSystemService",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[JValue::Object(&service_name)],
            )
            .map_err(|e| e.to_string())?
            .l()
            .map_err(|e| e.to_string())?;
        if wifi.is_null() {
            return Err("WifiManager unavailable".to_string());
        }
        let tag = env
            .new_string("tunnelmanager-mdns")
            .map_err(|e| e.to_string())?;
        let lock = env
            .call_method(
                &wifi,
                "createMulticastLock",
                "(Ljava/lang/String;)Landroid/net/wifi/WifiManager$MulticastLock;",
                &[JValue::Object(&tag)],
            )
            .map_err(|e| e.to_string())?
            .l()
            .map_err(|e| e.to_string())?;
        // Reference-counted off: a single acquire holds until release, and the
        // lock is never released here — it lives for the whole process.
        env.call_method(&lock, "setReferenceCounted", "(Z)V", &[JValue::Bool(0)])
            .map_err(|e| e.to_string())?;
        env.call_method(&lock, "acquire", "()V", &[])
            .map_err(|e| e.to_string())?;
        env.new_global_ref(lock).map_err(|e| e.to_string())
    }

    // ----------------------------------------------------------- clipboard

    pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
        let app = app().clone();
        let text = text.to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        app.run_on_java_main_thread(Box::new(move || {
            let _ = tx.send(with_env(|env| clipboard_with_env(env, &text)));
        }));
        rx.recv()
            .map_err(|_| "clipboard task did not run".to_string())?
    }

    fn clipboard_with_env(env: &mut jni::JNIEnv, text: &str) -> Result<(), String> {
        let activity = activity()?;
        // Context.CLIPBOARD_SERVICE == "clipboard"
        let service = env.new_string("clipboard").map_err(|e| e.to_string())?;
        let cm = env
            .call_method(
                &activity,
                "getSystemService",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[JValue::Object(&service)],
            )
            .map_err(|e| e.to_string())?
            .l()
            .map_err(|e| e.to_string())?;
        let clip_class = env
            .find_class("android/content/ClipData")
            .map_err(|e| e.to_string())?;
        let label = env.new_string("TunnelManager").map_err(|e| e.to_string())?;
        let jtext = env.new_string(text).map_err(|e| e.to_string())?;
        // ClipData.newPlainText(label, text)
        let clip = env
            .call_static_method(
                clip_class,
                "newPlainText",
                "(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Landroid/content/ClipData;",
                &[JValue::Object(&label), JValue::Object(&jtext)],
            )
            .map_err(|e| e.to_string())?
            .l()
            .map_err(|e| e.to_string())?;
        env.call_method(
            &cm,
            "setPrimaryClip",
            "(Landroid/content/ClipData;)V",
            &[JValue::Object(&clip)],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    // -------------------------------------------------------------- toasts

    pub fn show_toast(msg: &str, _error: bool) {
        let Some(app) = ANDROID_APP.get() else {
            return;
        };
        let msg = msg.to_string();
        // Fire-and-forget: blocking here would deadlock the UI thread when
        // called from inside event-loop closures.
        app.run_on_java_main_thread(Box::new(move || {
            if let Err(e) = with_env(|env| toast_with_env(env, &msg)) {
                tracing::warn!("toast failed: {e}");
            }
        }));
    }

    fn toast_with_env(env: &mut jni::JNIEnv, msg: &str) -> Result<(), String> {
        let activity = activity()?;
        let toast_class = env
            .find_class("android/widget/Toast")
            .map_err(|e| e.to_string())?;
        let jmsg = env.new_string(msg).map_err(|e| e.to_string())?;
        // Toast.makeText(activity, text, Toast.LENGTH_LONG).show();
        let toast = env
            .call_static_method(
                toast_class,
                "makeText",
                "(Landroid/content/Context;Ljava/lang/CharSequence;I)Landroid/widget/Toast;",
                &[
                    JValue::Object(&activity),
                    JValue::Object(&jmsg),
                    JValue::Int(1),
                ],
            )
            .map_err(|e| e.to_string())?
            .l()
            .map_err(|e| e.to_string())?;
        env.call_method(&toast, "show", "()V", &[])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    // ----------------------------------------------------- shared content

    /// Stage content shared into the app via the system share sheet.
    ///
    /// A lazy SAF picker needs activity-result plumbing that
    /// android-activity does not forward, so "Send to TunnelManager" from
    /// the system share sheet is the supported flow on Android. Files are
    /// copied into the outbox and the share uses those ordinary paths.
    pub fn pick_shared_files() -> Vec<PathBuf> {
        let app = app().clone();
        let (tx, rx) = std::sync::mpsc::channel();
        app.run_on_java_main_thread(Box::new(move || {
            let _ = tx.send(with_env(collect_shared_files_with_env));
        }));
        match rx.recv() {
            Ok(Ok(paths)) => paths,
            Ok(Err(e)) => {
                tracing::warn!("staging shared files failed: {e}");
                Vec::new()
            }
            Err(_) => {
                tracing::warn!("file collection task did not run");
                Vec::new()
            }
        }
    }

    /// Shares-into-the-app entry point ("intent listener"): stages whatever the
    /// launch intent carries (ACTION_SEND / ACTION_SEND_MULTIPLE) into the
    /// outbox, then returns every staged file. Called once at startup (so a
    /// share launched from another app prompts immediately) and again from the
    /// send button; a process-local guard keeps the same intent from being
    /// staged twice. NOTE: sharing while the app is already running in the
    /// background cannot be observed — android-activity does not forward
    /// `onNewIntent` — so that case restarts the activity with the share.
    pub fn pick_send_files() -> Vec<PathBuf> {
        let first = INTENT_STAGED.set(()).map(|_| true).unwrap_or(false);
        if first {
            pick_shared_files();
        }
        let dir = outbox_dir();
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| e.path().is_file())
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        files
    }

    fn collect_shared_files_with_env(env: &mut jni::JNIEnv) -> Result<Vec<PathBuf>, String> {
        let activity = activity()?;
        let intent = env
            .call_method(&activity, "getIntent", "()Landroid/content/Intent;", &[])
            .map_err(|e| e.to_string())?
            .l()
            .map_err(|e| e.to_string())?;
        if intent.is_null() {
            return Ok(Vec::new());
        }
        let action_obj = env
            .call_method(&intent, "getAction", "()Ljava/lang/String;", &[])
            .map_err(|e| e.to_string())?
            .l()
            .map_err(|e| e.to_string())?;
        let action = if action_obj.is_null() {
            String::new()
        } else {
            env.get_string(&JString::from(action_obj))
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .into_owned()
        };

        let key = env
            .new_string("android.intent.extra.STREAM")
            .map_err(|e| e.to_string())?;
        let mut uris: Vec<JObject> = Vec::new();
        match action.as_str() {
            // ACTION_SEND: a single Uri stream.
            "android.intent.action.SEND" => {
                let extra = env
                    .call_method(
                        &intent,
                        "getParcelableExtra",
                        "(Ljava/lang/String;)Landroid/os/Parcelable;",
                        &[JValue::Object(&key)],
                    )
                    .map_err(|e| e.to_string())?
                    .l()
                    .map_err(|e| e.to_string())?;
                if !extra.is_null() {
                    uris.push(extra);
                }
            }
            // ACTION_SEND_MULTIPLE: an ArrayList<Uri>.
            "android.intent.action.SEND_MULTIPLE" => {
                let list = env
                    .call_method(
                        &intent,
                        "getSerializableExtra",
                        "(Ljava/lang/String;)Ljava/io/Serializable;",
                        &[JValue::Object(&key)],
                    )
                    .map_err(|e| e.to_string())?
                    .l()
                    .map_err(|e| e.to_string())?;
                if !list.is_null() {
                    let size = env
                        .call_method(&list, "size", "()I", &[])
                        .map_err(|e| e.to_string())?
                        .i()
                        .map_err(|e| e.to_string())?;
                    for i in 0..size {
                        let item = env
                            .call_method(&list, "get", "(I)Ljava/lang/Object;", &[JValue::Int(i)])
                            .map_err(|e| e.to_string())?
                            .l()
                            .map_err(|e| e.to_string())?;
                        if !item.is_null() {
                            uris.push(item);
                        }
                    }
                }
            }
            _ => {}
        }
        if uris.is_empty() {
            return Ok(Vec::new());
        }
        stage_uris(env, uris)
    }

    fn stage_uris(env: &mut jni::JNIEnv, uris: Vec<JObject>) -> Result<Vec<PathBuf>, String> {
        let activity = activity()?;
        let resolver = env
            .call_method(
                &activity,
                "getContentResolver",
                "()Landroid/content/ContentResolver;",
                &[],
            )
            .map_err(|e| e.to_string())?
            .l()
            .map_err(|e| e.to_string())?;
        let mut staged = Vec::new();
        for uri in &uris {
            let name = uri_display_name(env, uri)?;
            let input = env
                .call_method(
                    &resolver,
                    "openInputStream",
                    "(Landroid/net/Uri;)Ljava/io/InputStream;",
                    &[JValue::Object(uri)],
                )
                .map_err(|e| e.to_string())?
                .l()
                .map_err(|e| e.to_string())?;
            if input.is_null() {
                continue;
            }
            let out_path = unique_outbox_path(&name);
            let result = copy_stream_to_file(env, &input, &out_path);
            let _ = env.call_method(&input, "close", "()V", &[]);
            result?;
            staged.push(out_path);
        }
        Ok(staged)
    }

    fn uri_display_name(env: &mut jni::JNIEnv, uri: &JObject) -> Result<String, String> {
        let seg = env
            .call_method(uri, "getLastPathSegment", "()Ljava/lang/String;", &[])
            .map_err(|e| e.to_string())?
            .l()
            .map_err(|e| e.to_string())?;
        if seg.is_null() {
            return Ok("shared-file".to_string());
        }
        Ok(env
            .get_string(&JString::from(seg))
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .into_owned())
    }

    fn unique_outbox_path(file_name: &str) -> PathBuf {
        let dir = outbox_dir();
        let safe: String = file_name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let mut dest = dir.join(&safe);
        let mut n = 1;
        while dest.exists() {
            let stem = Path::new(&safe)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| safe.clone());
            let ext = Path::new(&safe)
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_default();
            dest = dir.join(if ext.is_empty() {
                format!("{stem}-{n}")
            } else {
                format!("{stem}-{n}.{ext}")
            });
            n += 1;
        }
        dest
    }

    fn copy_stream_to_file(
        env: &mut jni::JNIEnv,
        input: &JObject,
        dest: &Path,
    ) -> Result<(), String> {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        use std::io::Write;
        let file = std::fs::File::create(dest).map_err(|e| e.to_string())?;
        let mut writer = std::io::BufWriter::new(file);
        let buf = env.new_byte_array(64 * 1024).map_err(|e| e.to_string())?;
        let buf_obj = buf.as_ref();
        loop {
            let n = env
                .call_method(input, "read", "([B)I", &[JValue::Object(buf_obj)])
                .map_err(|e| e.to_string())?
                .i()
                .map_err(|e| e.to_string())?;
            if n <= 0 {
                break;
            }
            let mut chunk = vec![0i8; n as usize];
            env.get_byte_array_region(&buf, 0, &mut chunk)
                .map_err(|e| e.to_string())?;
            // jbyte is i8; reinterpret as raw bytes for the file.
            let bytes: Vec<u8> = chunk.iter().map(|b| *b as u8).collect();
            writer.write_all(&bytes).map_err(|e| e.to_string())?;
        }
        writer.flush().map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(target_os = "android")]
pub use imp::*;

#[cfg(not(target_os = "android"))]
mod imp {
    use std::path::PathBuf;

    /// No-op on desktop (the Android app handle is never set there).
    pub fn show_toast(_msg: &str, _error: bool) {}

    /// Always empty on desktop.
    pub fn outbox_count() -> usize {
        0
    }

    /// Unused on desktop.
    pub fn clear_outbox(_paths: &[PathBuf]) {}
}
#[cfg(not(target_os = "android"))]
pub use imp::*;

// ------------------------------------------------------------- entry point

#[cfg(target_os = "android")]
#[no_mangle]
fn android_main(app: slint::android::android_activity::AndroidApp) {
    slint::android::init(app.clone()).expect("Slint Android init");
    imp::set_app(app);
    imp::acquire_multicast_lock();
    crate::app::run();
}
