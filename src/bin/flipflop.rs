//! `flipflop`: the GUI's command line twin (see [`flipflop::cli`]).
//!
//! If the GUI (or another `serve`) already has the data dir open, the command runs there;
//! otherwise this process starts its own node on the dir, runs it and exits. `flipflop gui`
//! starts the GUI itself (`flipflop-gui`) instead.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use clap::Parser;
use flipflop::{
    cli::{self, Cli, Command, Ctx, Out},
    core::{Core, Event},
    ipc,
};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();
    let cli = Cli::parse();
    let dir = cli.dir.clone().unwrap_or_else(|| {
        dirs::data_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("flipflop")
    });
    let cwd = std::env::current_dir()?;
    let args: Vec<String> = std::env::args().collect();
    if matches!(cli.command, Command::Gui) {
        return open_gui(&dir, cli.dir.is_some().then(|| cwd.join(&dir)));
    }

    // The data dir belongs to one process at a time. If it is taken, ask its owner to run the
    // command; if it is free (or frees up while we wait), run it here.
    let started = Instant::now();
    loop {
        if Core::dir_in_use(&dir) {
            if ipc::reachable(&dir).await {
                if cli.downloads.is_some() {
                    eprintln!(
                        "(--downloads is ignored: the running app keeps its own download folder)"
                    );
                }
                return ipc::route(&dir, args, cwd).await;
            }
        } else {
            return run_here(&cli, &dir, cwd).await;
        }
        if started.elapsed() > Duration::from_secs(10) {
            bail!(
                "{} is in use by another flipflop that does not take commands; stop it, or set FLIPFLOP_DIR for a separate identity",
                dir.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Starts the GUI detached, on `dir` if one was given, unless something already has the dir open.
fn open_gui(dir: &Path, given: Option<PathBuf>) -> Result<()> {
    if Core::dir_in_use(dir) {
        println!(
            "{} is already open (by the app, or a `flipflop serve`)",
            dir.display()
        );
        return Ok(());
    }
    let mut cmd = match gui_exe() {
        Some(exe) => std::process::Command::new(exe),
        None if flatpak_installed() => {
            let mut cmd = std::process::Command::new("flatpak");
            cmd.args(["run", FLATPAK_ID]);
            cmd
        }
        None => bail!(
            "flipflop-gui not found: install the desktop app, or put flipflop-gui in your PATH"
        ),
    };
    if let Some(d) = given {
        cmd.env("FLIPFLOP_DIR", d);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Its own process group, so closing the terminal does not take the window with it.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    cmd.spawn()
        .map_err(|e| anyhow::anyhow!("starting {}: {e}", cmd.get_program().to_string_lossy()))?;
    Ok(())
}

/// The Flatpak's id (packaging/flatpak), which keeps the identity in the same dir as this client.
const FLATPAK_ID: &str = "dev.flipflop.app";

fn flatpak_installed() -> bool {
    cfg!(target_os = "linux")
        && std::process::Command::new("flatpak")
            .args(["info", FLATPAK_ID])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
}

/// The GUI launcher: `flipflop-gui` in PATH or next to this binary, else the installed app (whose
/// launcher is named flipflop, like its shortcuts). The Flatpak is tried after these.
fn gui_exe() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "flipflop-gui.exe"
    } else {
        "flipflop-gui"
    };
    let mut places: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).map(|d| d.join(name)).collect())
        .unwrap_or_default();
    if let Some(d) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        places.push(d.join(name));
    }
    if cfg!(target_os = "linux") {
        places.push("/opt/flipflop/bin/flipflop".into());
    }
    if cfg!(windows)
        && let Some(d) = std::env::var_os("LOCALAPPDATA")
    {
        places.push(Path::new(&d).join("flipflop").join("flipflop.exe"));
    }
    if cfg!(target_os = "macos") {
        places.push("/Applications/flipflop.app/Contents/MacOS/flipflop".into());
        if let Some(home) = dirs::home_dir() {
            places.push(home.join("Applications/flipflop.app/Contents/MacOS/flipflop"));
        }
    }
    places.into_iter().find(|p| p.is_file())
}

async fn run_here(cli: &Cli, dir: &std::path::Path, cwd: std::path::PathBuf) -> Result<()> {
    let serving = matches!(cli.command, Command::Serve { .. });
    let core = Core::start(dir, false, move |event| {
        if serving {
            print_event(&event);
        }
    })
    .await?;
    if serving {
        // Other CLI invocations hand their commands to this node while it runs.
        ipc::serve(dir, core.clone())?;
    }
    if let Some(d) = &cli.downloads {
        core.set_download_dir(d.clone());
    }
    let cx = Ctx {
        out: Out::stdout(),
        cwd,
        routed: false,
    };
    let result = cli::run(&core, &cx, cli).await;
    core.shutdown().await;
    result
}

fn print_event(event: &Event) {
    match event {
        Event::Notice(n) => println!("· {n}"),
        Event::Group(v) => println!(
            "· group {}: {} files, {} texts, {} chars on the scratchpad",
            &v.id[..8],
            v.files.len(),
            v.texts.len(),
            v.scratch.chars().count()
        ),
        _ => {}
    }
}
