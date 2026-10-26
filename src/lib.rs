pub mod ble;
#[cfg(not(target_os = "android"))]
pub mod cli;
pub mod core;
pub mod format;
pub mod ffi;
pub mod groupdoc;
pub mod haiku;
#[cfg(not(target_os = "android"))]
pub mod ipc;
pub mod platform;
pub mod qr;
#[cfg(not(target_os = "android"))]
pub mod scan;
pub mod settings;
pub mod state;

uniffi::setup_scaffolding!();
