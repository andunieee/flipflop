//! `flipflop-cli`: the GUI's command line twin (see [`flipflop::cli`]).
//!
//! If the GUI (or another `serve`) already has the data dir open, the command runs there;
//! otherwise this process starts its own node on the dir, runs it and exits.

use std::time::{Duration, Instant};

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
