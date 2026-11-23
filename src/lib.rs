/// The crate version, as the packages and the Settings page show it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod apps;
pub mod ble;
#[cfg(not(target_os = "android"))]
pub mod cli;
pub mod core;
pub mod format;
pub mod groupdoc;
pub mod haiku;
#[cfg(not(target_os = "android"))]
pub mod ipc;
pub mod platform;
pub mod qr;
pub mod settings;
pub mod state;
