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
