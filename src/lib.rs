pub mod app;
pub mod ble;
pub mod core;
pub mod haiku;
pub mod platform;
pub mod qr;
#[cfg(not(target_os = "android"))]
pub mod scan;
pub mod settings;
pub mod state;

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(android_app: slint::android::AndroidApp) {
    // Quiet by default; `adb shell setprop debug.flipflop.log "warn,iroh_ble_transport=debug"` turns up
    // individual crates (takes effect on the next launch).
    let filter = platform::system_property("debug.flipflop.log").unwrap_or_else(|| "warn".into());
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Trace)
            .with_filter(android_logger::FilterBuilder::new().parse(&filter).build())
            .with_tag("flipflop"),
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
