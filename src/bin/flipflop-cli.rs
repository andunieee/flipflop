//! Command line twin of the GUI: the same node, the same groups, no window.
//!
//! Every command starts a node with the identity stored in `--dir`, waits for the peers it needs,
//! does its job and lets the changes sync before it exits. `serve` keeps the node running instead
//! (downloading, serving files, printing what happens), which is what makes this device a long-lived
//! group member.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use flipflop::{
    core::{Core, Event, GroupInfo, GroupView, MemberState, Peer},
    format,
};
use iroh::EndpointId;
use tokio::time::sleep;

#[derive(Parser)]
#[command(name = "flipflop-cli", about = "flipflop from the command line: groups with files, texts and a scratchpad")]
struct Cli {
    /// Where this node keeps its identity and groups.
    #[arg(long, env = "FLIPFLOP_DIR", global = true)]
    dir: Option<PathBuf>,
    /// Where downloaded files go (default: Downloads/flipflop).
    #[arg(long, env = "FLIPFLOP_DOWNLOADS", global = true)]
    downloads: Option<PathBuf>,
    /// How long (seconds) to wait for peers and for changes to sync.
    #[arg(long, default_value_t = 8, global = true)]
    wait: u64,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print this node's id, name and ticket.
    Id,
    /// Show or change this device's name.
    Name { new: Option<String> },
    /// List peers: nearby ones and saved contacts.
    Peers,
    /// Connect to a peer by ticket (or endpoint id) and save it as a contact.
    Connect { ticket: String },
    /// Give a contact a local name (empty to remove it).
    Rename { peer: String, alias: String },
    /// Remove a contact.
    Forget { peer: String },
    /// Groups: create, invite, accept, settings.
    #[command(subcommand)]
    Group(GroupCmd),
    /// Files offered to a group.
    #[command(subcommand)]
    File(FileCmd),
    /// Texts (links, codes, anything) shared in a group.
    #[command(subcommand)]
    Text(TextCmd),
    /// The group's scratchpad.
    #[command(subcommand)]
    Pad(PadCmd),
    /// WebAssembly apps (not implemented yet).
    Apps,
    /// Keep running: sync, download, serve files, print events. Stops after --secs, or on Ctrl-C.
    Serve {
        #[arg(long)]
        secs: Option<u64>,
    },
}

#[derive(Subcommand)]
enum GroupCmd {
    /// List groups and pending invitations.
    List,
    /// Create a group.
    New,
    /// Members, settings and counts of one group.
    Show { group: String },
    /// Local name for a group (empty to remove it).
    Rename { group: String, alias: String },
    /// Leave (delete locally) a group.
    Leave { group: String },
    /// Invite a contact.
    Invite { group: String, peer: String },
    /// Accept an invitation (group id, haiku or inviter name).
    Accept { invite: String },
    /// Decline an invitation.
    Decline { invite: String },
    /// Per-group settings on this device.
    Settings {
        group: String,
        /// Download every offered file automatically.
        #[arg(long)]
        auto_download: Option<bool>,
        /// Stop sharing a file once everybody has downloaded it.
        #[arg(long)]
        auto_stop: Option<bool>,
    },
}

#[derive(Subcommand)]
enum FileCmd {
    List { group: String },
    /// Offer a file; stays online until everyone has it or --wait runs out.
    Offer {
        group: String,
        path: PathBuf,
        /// "file", "media" or "apk".
        #[arg(long, default_value = "file")]
        kind: String,
        /// Name to show instead of the file's own.
        #[arg(long)]
        name: Option<String>,
    },
    /// Stop sharing one of your files.
    Stop { group: String, file: String },
    /// Download a file now.
    Get { group: String, file: String },
}

#[derive(Subcommand)]
enum TextCmd {
    List { group: String },
    Send { group: String, text: String },
    Delete { group: String, id: String },
}

#[derive(Subcommand)]
enum PadCmd {
    Show { group: String },
    /// Replace the scratchpad's text.
    Set { group: String, text: String },
    /// Add a line at the end.
    Append { group: String, text: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into())).init();
    let cli = Cli::parse();
    let dir = cli.dir.clone().unwrap_or_else(|| dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("flipflop-cli"));
    let printing = matches!(cli.command, Command::Serve { .. });
    let core = Core::start(&dir, false, move |event| {
        if printing {
            print_event(&event);
        }
    })
    .await?;
    if let Some(d) = &cli.downloads {
        core.set_download_dir(d.clone());
    }
    let result = run(&core, &cli).await;
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

async fn run(core: &Core, cli: &Cli) -> Result<()> {
    let wait = Duration::from_secs(cli.wait);
    match &cli.command {
        Command::Id => {
            wait_for(wait, || !core.ticket().is_empty()).await;
            println!("id      {}", core.id());
            println!("name    {}", core.name());
            println!("ticket  {}", core.ticket());
        }
        Command::Name { new } => {
            if let Some(new) = new {
                core.set_name(new);
            }
            println!("{}", core.name());
        }
        Command::Peers => {
            sleep(wait.min(Duration::from_secs(4))).await;
            for p in core.peers() {
                println!("{}", peer_line(&p));
            }
        }
        Command::Connect { ticket } => {
            let id = match ticket.parse::<EndpointId>() {
                Ok(id) => {
                    core.connect(id).await?;
                    id
                }
                Err(_) => core.connect_ticket(ticket).await?,
            };
            println!("connected to {}", id.fmt_short());
            sleep(Duration::from_secs(1)).await;
        }
        Command::Rename { peer, alias } => {
            let id = resolve_peer(core, peer, wait).await?;
            core.rename_contact(id, alias);
        }
        Command::Forget { peer } => {
            let id = resolve_peer(core, peer, wait).await?;
            core.forget_contact(id);
        }
        Command::Group(cmd) => group(core, cmd, wait).await?,
        Command::File(cmd) => file(core, cmd, wait).await?,
        Command::Text(cmd) => text(core, cmd, wait).await?,
        Command::Pad(cmd) => pad(core, cmd, wait).await?,
        Command::Apps => println!("Apps are not implemented yet: groups will share WebAssembly apps with a hypereffect.dev interface."),
        Command::Serve { secs } => {
            println!("serving as {} ({})", core.name(), core.id());
            wait_for(Duration::from_secs(10), || !core.ticket().is_empty()).await;
            println!("ticket {}", core.ticket());
            match secs {
                Some(s) => sleep(Duration::from_secs(*s)).await,
                None => {
                    let _ = tokio::signal::ctrl_c().await;
                }
            }
        }
    }
    Ok(())
}

// ---- groups ---------------------------------------------------------------------------

async fn group(core: &Core, cmd: &GroupCmd, wait: Duration) -> Result<()> {
    match cmd {
        GroupCmd::List => {
            // Invites that were waiting for us arrive when the inviter connects.
            sleep(wait.min(Duration::from_secs(4))).await;
            let view = core.groups();
            for g in &view.groups {
                println!("{}  {}  {}", &g.id[..8], title(g), summary(g));
            }
            for i in &view.invites {
                println!("invite  {}  from {}  (flipflop-cli group accept {})", i.haiku, i.inviter_name, &i.group[..8]);
            }
            if view.groups.is_empty() && view.invites.is_empty() {
                println!("no groups");
            }
        }
        GroupCmd::New => {
            let id = core.create_group();
            let info = find_group(core, &id)?;
            println!("{}  {}", id, info.haiku);
        }
        GroupCmd::Show { group } => {
            let info = find_group(core, group)?;
            settle(core, &info.id, wait).await;
            let v = core.group_view(&info.id).context("group vanished")?;
            println!("{}", title(&info));
            println!("id            {}", info.id);
            println!("haiku         {}", info.haiku);
            println!("auto-download {}", info.auto_download);
            println!("auto-stop     {}", info.auto_stop);
            println!("members:");
            for m in &v.members {
                println!("  {}  {}{}", m.id.chars().take(8).collect::<String>(), m.name, if m.me { "  (this device)" } else if m.online { "  online" } else { "" });
            }
            for m in info.members.iter().filter(|m| m.state != MemberState::Accepted && !v.members.iter().any(|d| d.id == m.id.to_string())) {
                println!("  {}  {}  {:?}", m.id.fmt_short(), m.name, m.state);
            }
            println!("{} files, {} texts, scratchpad {} chars", v.files.len(), v.texts.len(), v.scratch.chars().count());
        }
        GroupCmd::Rename { group, alias } => {
            let info = find_group(core, group)?;
            core.rename_group(&info.id, alias);
        }
        GroupCmd::Leave { group } => {
            let info = find_group(core, group)?;
            core.delete_group(&info.id);
        }
        GroupCmd::Invite { group, peer } => {
            let info = find_group(core, group)?;
            let id = resolve_peer(core, peer, wait).await?;
            core.add_to_group(&info.id, id);
            wait_for(wait, || {
                core.groups().groups.iter().find(|g| g.id == info.id).is_some_and(|g| {
                    g.members.iter().any(|m| m.id == id && m.state != MemberState::Pending)
                })
            })
            .await;
            let state = core.groups().groups.iter().find(|g| g.id == info.id).and_then(|g| g.members.iter().find(|m| m.id == id).map(|m| m.state));
            println!("invitation {:?}", state.unwrap_or(MemberState::Pending));
            // Stay a moment: an answer may come right back.
            wait_for(wait, || {
                core.groups().groups.iter().find(|g| g.id == info.id).is_some_and(|g| {
                    g.members.iter().any(|m| m.id == id && matches!(m.state, MemberState::Accepted | MemberState::Declined))
                })
            })
            .await;
            let state = core.groups().groups.iter().find(|g| g.id == info.id).and_then(|g| g.members.iter().find(|m| m.id == id).map(|m| m.state));
            println!("{} is now {:?}", peer, state.unwrap_or(MemberState::Pending));
        }
        GroupCmd::Accept { invite } | GroupCmd::Decline { invite } => {
            let accept = matches!(cmd, GroupCmd::Accept { .. });
            wait_for(wait, || find_invite(core, invite).is_ok()).await;
            let (inviter, gid) = find_invite(core, invite)?;
            if accept {
                core.accept_invite(inviter, &gid);
            } else {
                core.reject_invite(inviter, &gid);
            }
            // Let the answer (and the documents, on accept) go out and come back.
            sleep(Duration::from_secs(if accept { 4 } else { 2 })).await;
            println!("{}", if accept { "accepted" } else { "declined" });
        }
        GroupCmd::Settings { group, auto_download, auto_stop } => {
            let info = find_group(core, group)?;
            core.set_group_settings(&info.id, *auto_download, *auto_stop);
            let info = find_group(core, &info.id)?;
            println!("auto-download {}", info.auto_download);
            println!("auto-stop     {}", info.auto_stop);
        }
    }
    Ok(())
}

// ---- files ----------------------------------------------------------------------------

async fn file(core: &Core, cmd: &FileCmd, wait: Duration) -> Result<()> {
    match cmd {
        FileCmd::List { group } => {
            let gid = find_group(core, group)?.id;
            settle(core, &gid, wait).await;
            let v = core.group_view(&gid).context("group vanished")?;
            if v.files.is_empty() {
                println!("no files");
            }
            for f in &v.files {
                println!("{}", file_line(f));
            }
        }
        FileCmd::Offer { group, path, kind, name } => {
            let gid = find_group(core, group)?.id;
            settle(core, &gid, wait.min(Duration::from_secs(6))).await;
            let path = std::fs::canonicalize(path).with_context(|| format!("no such file: {}", path.display()))?;
            let fid = core.offer_file(&gid, path, name.clone(), kind, false).await?;
            println!("offered {fid}");
            // Serve until everybody else has it (or the timeout), so the transfer can happen.
            let started = Instant::now();
            loop {
                let v = core.group_view(&gid).context("group vanished")?;
                let f = v.files.iter().find(|f| f.id == fid).context("offer vanished")?;
                if f.stopped || (f.audience > 0 && f.downloaded_by.len() >= f.audience) {
                    println!("{}", file_line(f));
                    break;
                }
                if started.elapsed() > wait {
                    println!("{}", file_line(f));
                    println!("(still on offer: keep a node running with `serve` so members can download it)");
                    break;
                }
                sleep(Duration::from_millis(300)).await;
            }
        }
        FileCmd::Stop { group, file } => {
            let gid = find_group(core, group)?.id;
            let fid = find_file(core, &gid, file)?;
            core.stop_file(&gid, &fid)?;
            sleep(Duration::from_secs(3)).await;
            println!("stopped");
        }
        FileCmd::Get { group, file } => {
            let gid = find_group(core, group)?.id;
            settle(core, &gid, wait).await;
            let fid = find_file(core, &gid, file)?;
            let path = core.download_file(&gid, &fid).await?;
            println!("saved to {}", path.display());
            sleep(Duration::from_secs(2)).await;
        }
    }
    Ok(())
}

// ---- texts and scratchpad -------------------------------------------------------------

async fn text(core: &Core, cmd: &TextCmd, wait: Duration) -> Result<()> {
    match cmd {
        TextCmd::List { group } => {
            let gid = find_group(core, group)?.id;
            settle(core, &gid, wait).await;
            let v = core.group_view(&gid).context("group vanished")?;
            if v.texts.is_empty() {
                println!("no texts");
            }
            for t in v.texts.iter().rev() {
                println!("{}  {}  {}{}", &t.id[..t.id.len().min(13)], format::time(t.ts), t.name, if t.mine { " (you)" } else { "" });
                for line in t.text.lines() {
                    println!("    {line}");
                }
            }
        }
        TextCmd::Send { group, text } => {
            let gid = find_group(core, group)?.id;
            settle(core, &gid, wait.min(Duration::from_secs(5))).await;
            core.send_text(&gid, text)?;
            sleep(Duration::from_secs(3)).await;
            println!("sent");
        }
        TextCmd::Delete { group, id } => {
            let gid = find_group(core, group)?.id;
            settle(core, &gid, wait.min(Duration::from_secs(5))).await;
            let v = core.group_view(&gid).context("group vanished")?;
            let hits: Vec<_> = v.texts.iter().filter(|t| t.id.starts_with(id.as_str())).collect();
            let [t] = hits.as_slice() else { bail!("{} snippets match {id:?}", hits.len()) };
            core.delete_text(&gid, &t.id.clone())?;
            sleep(Duration::from_secs(3)).await;
            println!("deleted");
        }
    }
    Ok(())
}

async fn pad(core: &Core, cmd: &PadCmd, wait: Duration) -> Result<()> {
    match cmd {
        PadCmd::Show { group } => {
            let gid = find_group(core, group)?.id;
            settle(core, &gid, wait).await;
            println!("{}", core.scratch(&gid).unwrap_or_default());
        }
        PadCmd::Set { group, text } => {
            let gid = find_group(core, group)?.id;
            settle(core, &gid, wait.min(Duration::from_secs(5))).await;
            core.set_scratch(&gid, text);
            sleep(Duration::from_secs(3)).await;
        }
        PadCmd::Append { group, text } => {
            let gid = find_group(core, group)?.id;
            settle(core, &gid, wait.min(Duration::from_secs(5))).await;
            let mut current = core.scratch(&gid).unwrap_or_default();
            if !current.is_empty() && !current.ends_with('\n') {
                current.push('\n');
            }
            current.push_str(text);
            current.push('\n');
            core.set_scratch(&gid, &current);
            sleep(Duration::from_secs(3)).await;
        }
    }
    Ok(())
}

// ---- helpers --------------------------------------------------------------------------

async fn wait_for(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let started = Instant::now();
    while !done() {
        if started.elapsed() > limit {
            return false;
        }
        sleep(Duration::from_millis(200)).await;
    }
    true
}

/// Wait until some other member of the group is connected, then give the sync a moment.
async fn settle(core: &Core, gid: &str, limit: Duration) {
    let others_online = || core.group_view(gid).is_some_and(|v| v.members.iter().any(|m| !m.me && m.online));
    if wait_for(limit, others_online).await {
        sleep(Duration::from_millis(2500)).await;
    }
}

fn title(g: &GroupInfo) -> String {
    g.alias.clone().unwrap_or_else(|| g.haiku.clone())
}

fn summary(g: &GroupInfo) -> String {
    let n = |s: MemberState| g.members.iter().filter(|m| m.state == s).count();
    format!("{} joined, {} invited, {} declined", n(MemberState::Accepted), n(MemberState::Sent) + n(MemberState::Pending), n(MemberState::Declined))
}

fn peer_line(p: &Peer) -> String {
    let mut tags = vec![];
    if p.connected {
        tags.push("connected");
    }
    if p.saved {
        tags.push("saved");
    }
    if p.lan {
        tags.push("lan");
    }
    if p.ble {
        tags.push("ble");
    }
    let name = match &p.alias {
        Some(a) => format!("{a} ({})", p.name),
        None => p.name.clone(),
    };
    format!("{}  {}  [{}]", p.id.fmt_short(), name, tags.join(" "))
}

fn file_line(f: &flipflop::core::FileView) -> String {
    let state = if f.stopped {
        "stopped".to_string()
    } else if f.mine {
        format!("sharing, {} of {} have it", f.downloaded_by.len(), f.audience)
    } else if f.have {
        format!("downloaded{}", f.saved_to.as_ref().map(|p| format!(" to {}", p.display())).unwrap_or_default())
    } else if let Some((done, total)) = f.progress {
        format!("downloading {}/{}", format::size(done), format::size(total))
    } else if f.offerer_online {
        "available".to_string()
    } else {
        "sender offline".to_string()
    };
    format!("{}  {}  {}  from {}  {}", f.id, f.name, format::size(f.size), if f.mine { "you" } else { &f.from_name }, state)
}

fn find_group(core: &Core, query: &str) -> Result<GroupInfo> {
    let groups = core.groups().groups;
    let q = query.to_lowercase();
    let exact: Vec<&GroupInfo> = groups.iter().filter(|g| g.id == query || g.alias.as_deref().is_some_and(|a| a.to_lowercase() == q)).collect();
    let hits: Vec<&GroupInfo> = if exact.len() == 1 {
        exact
    } else {
        groups
            .iter()
            .filter(|g| g.id.starts_with(&q) || g.haiku.to_lowercase().contains(&q) || g.alias.as_deref().is_some_and(|a| a.to_lowercase().contains(&q)))
            .collect()
    };
    match hits.as_slice() {
        [g] => Ok((*g).clone()),
        [] => Err(anyhow!("no group matches {query:?} (see `group list`)")),
        _ => Err(anyhow!("{} groups match {query:?}, be more specific", hits.len())),
    }
}

/// Peers show up as discovery finds them, so a fresh process waits for a match.
async fn resolve_peer(core: &Core, query: &str, wait: Duration) -> Result<EndpointId> {
    let started = Instant::now();
    loop {
        match find_peer(core, query) {
            Ok(id) => return Ok(id),
            Err(err) if started.elapsed() > wait || err.to_string().contains("be more specific") => return Err(err),
            Err(_) => sleep(Duration::from_millis(250)).await,
        }
    }
}

fn find_peer(core: &Core, query: &str) -> Result<EndpointId> {
    let peers = core.peers();
    let q = query.to_lowercase();
    let hits: Vec<&Peer> = peers
        .iter()
        .filter(|p| {
            let id = p.id.to_string();
            id.starts_with(&q) || p.name.to_lowercase().contains(&q) || p.alias.as_deref().is_some_and(|a| a.to_lowercase().contains(&q))
        })
        .collect();
    match hits.as_slice() {
        [p] => Ok(p.id),
        [] => query.parse().map_err(|_| anyhow!("no peer matches {query:?} (see `peers`)")),
        _ => Err(anyhow!("{} peers match {query:?}, be more specific", hits.len())),
    }
}

fn find_invite(core: &Core, query: &str) -> Result<(EndpointId, String)> {
    let q = query.to_lowercase();
    let invites = core.groups().invites;
    let hits: Vec<_> = invites
        .iter()
        .filter(|i| i.group.starts_with(&q) || i.haiku.to_lowercase().contains(&q) || i.inviter_name.to_lowercase().contains(&q))
        .collect();
    match hits.as_slice() {
        [i] => Ok((i.inviter, i.group.clone())),
        [] => Err(anyhow!("no invitation matches {query:?}")),
        _ => Err(anyhow!("{} invitations match {query:?}", hits.len())),
    }
}

fn find_file(core: &Core, gid: &str, query: &str) -> Result<String> {
    let v: GroupView = core.group_view(gid).context("group vanished")?;
    let q = query.to_lowercase();
    let hits: Vec<_> = v.files.iter().filter(|f| f.id.starts_with(query) || f.name.to_lowercase().contains(&q)).collect();
    match hits.as_slice() {
        [f] => Ok(f.id.clone()),
        [] => Err(anyhow!("no file matches {query:?} (see `file list`)")),
        _ => Err(anyhow!("{} files match {query:?}, use the id", hits.len())),
    }
}
