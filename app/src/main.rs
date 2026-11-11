//! The desktop app.

// No console window behind the app on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;

use flipflop_app::{Backend, launch_app};
use freya::prelude::{LaunchConfig, launch};

const ICON: &[u8] = include_bytes!("../../packaging/icons/256.png");

/// Where the node keeps its data: `FLIPFLOP_DIR`, else the platform's data directory.
fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("FLIPFLOP_DIR") {
        return dir.into();
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("flipflop")
}

fn main() {
    if std::env::args().any(|a| a == "--install-desktop-entry") {
        if let Err(err) = install_desktop_entry() {
            eprintln!("Could not install the desktop entry: {err:#}");
            std::process::exit(1);
        }
        return;
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("FLIPFLOP_LOG")
                .unwrap_or_else(|_| "warn".into()),
        )
        .init();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    // Core calls from the UI spawn tasks, so the UI thread runs inside the runtime.
    let _guard = rt.enter();
    let backend = Backend::start(rt.handle().clone(), data_dir());
    launch(launch_app(backend.clone(), |window| {
        window
            .with_title("FlipFlop")
            .with_app_id("dev.flipflop.app")
            .with_icon(LaunchConfig::window_icon(ICON))
            .with_size(960., 660.)
            .with_min_size(640., 480.)
    }));
    backend.shutdown();
}

/// `--install-desktop-entry`: puts a flipflop that no package installed (the AppImage, the portable
/// folder) in the application menu, for this user.
fn install_desktop_entry() -> anyhow::Result<()> {
    use anyhow::{Context, bail};

    if !cfg!(target_os = "linux") {
        bail!("--install-desktop-entry only works on Linux");
    }
    // The AppImage itself rather than its temporary mount; otherwise this very binary.
    let exe = match std::env::var_os("APPIMAGE") {
        Some(path) => PathBuf::from(path),
        None => std::env::current_exe().context("finding this program")?,
    };
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/share")))
        .context("no home directory")?;
    let applications = data.join("applications");
    let icons = data.join("icons/hicolor");

    let template = include_str!("../../packaging/linux/dev.flipflop.app.desktop");
    let exec = format!("Exec={}", quote(&exe.to_string_lossy()));
    let entry_text: String = template
        .lines()
        .map(|line| if line == "Exec=flipflop" { exec.as_str() } else { line })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::create_dir_all(&applications)?;
    let entry = applications.join("dev.flipflop.app.desktop");
    std::fs::write(&entry, entry_text + "\n")?;
    let sizes: [(u32, &[u8]); 8] = [
        (16, include_bytes!("../../packaging/icons/16.png")),
        (24, include_bytes!("../../packaging/icons/24.png")),
        (32, include_bytes!("../../packaging/icons/32.png")),
        (48, include_bytes!("../../packaging/icons/48.png")),
        (64, include_bytes!("../../packaging/icons/64.png")),
        (128, include_bytes!("../../packaging/icons/128.png")),
        (256, include_bytes!("../../packaging/icons/256.png")),
        (512, include_bytes!("../../packaging/icons/512.png")),
    ];
    for (size, png) in sizes {
        let dir = icons.join(format!("{size}x{size}/apps"));
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("dev.flipflop.app.png"), png)?;
    }

    // Best effort: the entry works without these, just not right away.
    let _ = std::process::Command::new("update-desktop-database").arg(&applications).status();
    let _ = std::process::Command::new("gtk-update-icon-cache")
        .args(["-q", "-t"])
        .arg(&icons)
        .status();
    println!("Installed {}", entry.display());
    Ok(())
}

/// Quotes a path for the Exec key of a desktop entry.
fn quote(path: &str) -> String {
    let mut out = String::from('"');
    for c in path.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                out.push_str("\\\\");
                out.push(c);
            }
            '%' => out.push_str("%%"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}
