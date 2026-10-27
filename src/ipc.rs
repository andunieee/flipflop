//! Lets a `flipflop-cli` process hand its command to the app that already owns the data dir.
//!
//! The owner (the GUI, or a CLI running `serve`) listens on a Unix socket in the data dir (a named
//! pipe on Windows). A client sends one JSON line `{args, cwd}`; the owner parses the arguments
//! like the CLI would, runs the command against its own node, and answers with JSON lines:
//! `{"out": text}` for what the command prints, then `{"done": true}` or `{"err": message}`.

use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    sync::mpsc,
};

use crate::{
    cli::{self, Cli, Ctx, Out},
    core::Core,
};

#[derive(Serialize, Deserialize)]
struct Request {
    args: Vec<String>,
    cwd: PathBuf,
}

#[derive(Serialize, Deserialize, Default)]
struct Frame {
    #[serde(skip_serializing_if = "Option::is_none")]
    out: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    err: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    done: bool,
}

// ---- platform: where the endpoint lives ------------------------------------------------

#[cfg(unix)]
mod sys {
    use super::*;
    use tokio::net::{UnixListener, UnixStream};

    pub type Client = UnixStream;
    pub type Server = UnixStream;
    pub struct Listener(UnixListener);

    fn path(dir: &Path) -> PathBuf {
        dir.join("ipc.sock")
    }

    pub fn listen(dir: &Path) -> io::Result<Listener> {
        let path = path(dir);
        // We hold the data dir's lock, so a socket file still here is a leftover.
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Listener(listener))
    }

    impl Listener {
        pub async fn accept(&mut self) -> io::Result<Server> {
            Ok(self.0.accept().await?.0)
        }
    }

    pub async fn connect(dir: &Path) -> io::Result<Client> {
        UnixStream::connect(path(dir)).await
    }
}

#[cfg(windows)]
mod sys {
    use super::*;
    use tokio::net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
    };

    pub type Client = NamedPipeClient;
    pub type Server = NamedPipeServer;
    pub struct Listener {
        name: String,
        next: NamedPipeServer,
    }

    fn name(dir: &Path) -> String {
        let key = blake3::hash(dir.to_string_lossy().to_lowercase().as_bytes());
        format!(r"\\.\pipe\flipflop-{}", &key.to_hex()[..16])
    }

    pub fn listen(dir: &Path) -> io::Result<Listener> {
        let name = name(dir);
        let next = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)?;
        Ok(Listener { name, next })
    }

    impl Listener {
        pub async fn accept(&mut self) -> io::Result<Server> {
            self.next.connect().await?;
            let fresh = ServerOptions::new().create(&self.name)?;
            Ok(std::mem::replace(&mut self.next, fresh))
        }
    }

    pub async fn connect(dir: &Path) -> io::Result<Client> {
        ClientOptions::new().open(name(dir))
    }
}

// ---- the owner's side -----------------------------------------------------------------

/// Start answering commands from other processes. Needs a tokio runtime; the listener lives as
/// long as the runtime does.
pub fn serve(dir: &Path, core: Core) -> Result<()> {
    let mut listener =
        sys::listen(dir).with_context(|| format!("listening for commands in {}", dir.display()))?;
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok(stream) => {
                    tokio::spawn(handle(core.clone(), stream));
                }
                Err(err) => {
                    tracing::warn!("ipc accept: {err}");
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
    });
    Ok(())
}

/// A `Write` that turns whatever a command prints into `{"out"}` frames.
struct FrameWriter(mpsc::UnboundedSender<Frame>);

impl Write for FrameWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _ = self.0.send(Frame {
            out: Some(String::from_utf8_lossy(buf).into_owned()),
            ..Default::default()
        });
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

async fn handle(core: Core, stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static) {
    let (read, mut write) = tokio::io::split(stream);
    let mut line = String::new();
    if BufReader::new(read).read_line(&mut line).await.unwrap_or(0) == 0 {
        return;
    }
    let (tx, mut rx) = mpsc::unbounded_channel::<Frame>();
    let job = {
        let tx = tx.clone();
        async move {
            let request: Request = serde_json::from_str(&line).context("bad request")?;
            let cli = Cli::try_parse_from(&request.args).map_err(|e| anyhow!("{}", e.render()))?;
            let cx = Ctx {
                out: Out::new(FrameWriter(tx)),
                cwd: request.cwd,
                routed: true,
            };
            cli::run(&core, &cx, &cli).await
        }
    };
    tokio::spawn(async move {
        let last = match job.await {
            Ok(()) => Frame {
                done: true,
                ..Default::default()
            },
            Err(err) => Frame {
                err: Some(format!("{err:#}")),
                ..Default::default()
            },
        };
        let _ = tx.send(last);
    });
    // The sender inside the command's `Ctx` is gone once the command is, ending this loop after the
    // final frame.
    while let Some(frame) = rx.recv().await {
        let last = frame.done || frame.err.is_some();
        let mut text = serde_json::to_string(&frame).unwrap_or_default();
        text.push('\n');
        if write.write_all(text.as_bytes()).await.is_err() || last {
            break;
        }
    }
    let _ = write.shutdown().await;
}

// ---- the CLI's side -------------------------------------------------------------------

/// Run `args` (the CLI's own argv, without the program name resolved) in the app that owns `dir`.
/// Fails if nobody is listening.
pub async fn route(dir: &Path, args: Vec<String>, cwd: PathBuf) -> Result<()> {
    let stream = sys::connect(dir).await?;
    let (read, mut write) = tokio::io::split(stream);
    let mut request = serde_json::to_string(&Request { args, cwd })?;
    request.push('\n');
    write.write_all(request.as_bytes()).await?;
    let mut lines = BufReader::new(read).lines();
    let mut stdout = io::stdout();
    while let Some(line) = lines.next_line().await? {
        let frame: Frame =
            serde_json::from_str(&line).context("bad answer from the running app")?;
        if let Some(text) = frame.out {
            stdout.write_all(text.as_bytes())?;
            stdout.flush()?;
        }
        if let Some(err) = frame.err {
            return Err(anyhow!("{err}"));
        }
        if frame.done {
            return Ok(());
        }
    }
    Err(anyhow!(
        "the running app closed the connection before finishing"
    ))
}

/// Can something be asked to run commands in `dir`?
pub async fn reachable(dir: &Path) -> bool {
    sys::connect(dir).await.is_ok()
}
