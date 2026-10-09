//! Desktop entry point. All app logic lives in `app` so the Android build
//! (lib.rs → android.rs) shares it.

fn main() {
    flipflop::app::run();
}
