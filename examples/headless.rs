//! Headless flipflop node for debugging: prints everything it sees.
//!   cargo run --example headless [-- <ticket>]     connect to a ticket, say hi to every peer that connects
use std::time::Duration;

use flipflop::core::{Core, Event};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let dir = std::env::temp_dir().join("flipflop-headless");
    let core = Core::start(&dir, true, |event| match event {
        Event::Peers(peers) => {
            for p in peers {
                println!("peer {} {:?} alias={:?} saved={} lan={} ble={} connected={}", p.id, p.name, p.alias, p.saved, p.lan, p.ble, p.connected);
            }
            println!("--");
        }
        Event::Groups(v) => println!("groups {} invites {}", v.groups.len(), v.invites.len()),
        Event::Ticket(t) => println!("ticket {t}"),
        Event::Name(n) => println!("name {n:?}"),
        Event::Message { peer, mine, text } => println!("msg {} {} {text:?}", peer.fmt_short(), if mine { "->" } else { "<-" }),
        Event::Notice(n) => println!("notice {n}"),
    })
    .await?;
    flipflop::ble::start(core.clone(), |s| println!("ble: {s}"));
    if let Some(arg) = std::env::args().nth(1) {
        // a ticket, or an endpoint id previously seen on the LAN
        let id = match arg.parse::<iroh::EndpointId>() {
            Ok(id) => {
                core.connect(id).await?;
                id
            }
            Err(_) => core.connect_ticket(&arg).await?,
        };
        core.send(id, format!("hello from {}", core.name())).await?;
    }
    tokio::time::sleep(Duration::from_secs(
        std::env::var("SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(30),
    ))
    .await;
    core.shutdown().await;
    Ok(())
}
