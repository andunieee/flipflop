//! Desktop entry point. All app logic lives in `app` so the Android build
//! (lib.rs → android.rs) shares it.

// Release builds on Windows are GUI-subsystem apps, so no console window
// opens alongside them. Debug builds keep the console for logs.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    flipflop::app::run();
}
