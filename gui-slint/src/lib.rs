//! Shared UI + logic for the Slint frontend.
//!
//! The same code builds as the desktop binary (`tunnelmanager-slint`) and as
//! the cdylib loaded by the Android `NativeActivity` (see `android/` and the
//! README's Android section).

pub mod android;
pub mod app;
pub mod emitter;
pub mod format;
pub mod recorder;
pub mod settings;

slint::include_modules!();
