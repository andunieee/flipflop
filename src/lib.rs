pub mod app;
pub mod ble;
pub mod core;
pub mod platform;
pub mod qr;
#[cfg(not(target_os = "android"))]
pub mod scan;
pub mod settings;

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(android_app: slint::android::AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Warn).with_tag("flipflop"),
    );
    let dir = android_app
        .internal_data_path()
        .unwrap_or_else(|| std::path::PathBuf::from("/data/local/tmp/flipflop"));
    platform::init(android_app.activity_as_ptr());
    slint::android::init(android_app).unwrap();
    if let Err(err) = app::run(dir) {
        log::error!("flipflop exited: {err:#}");
    }
}
