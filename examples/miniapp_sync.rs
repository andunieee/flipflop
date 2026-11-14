//! Runs one miniapp on two peers: each has its own document, renderer and actions file, and they
//! sync like two devices. Fails if the two renderers end up showing different things.
//!
//! ```text
//! cargo run --example miniapp_sync -- examples/miniapps/dist/checklist.wasm a.json b.json
//! ```

#[path = "common/render.rs"]
mod render;

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use flipflop::apps::{App, DocHost, Runtime};
use flipflop::groupdoc::GroupDoc;

use render::Renderer;

struct Shared(Mutex<GroupDoc>);

impl DocHost for Shared {
    fn with_doc(&self, f: &mut dyn FnMut(&mut GroupDoc)) {
        f(&mut self.0.lock().unwrap())
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let wasm_path = args
        .next()
        .context("usage: miniapp_sync <app.wasm> <actions-a.json> <actions-b.json>")?;
    let a_path = args.next().context("missing actions-a.json")?;
    let b_path = args.next().context("missing actions-b.json")?;

    let wasm = std::fs::read(&wasm_path).with_context(|| format!("cannot read {wasm_path}"))?;
    let app_id = std::path::Path::new(&wasm_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .context("the wasm file has no stem")?
        .to_owned();
    let a_actions = render::read_actions(&a_path)?;
    let b_actions = render::read_actions(&b_path)?;

    let host_a = Arc::new(Shared(Mutex::new(GroupDoc::new())));
    let bytes = {
        let mut doc = host_a.0.lock().unwrap();
        doc.join("me", "Me");
        doc.save()
    };
    let mut b_doc = GroupDoc::load(&bytes)?;
    b_doc.join("bob", "Bob");
    let host_b = Arc::new(Shared(Mutex::new(b_doc)));

    let runtime = Runtime::default();
    let mut app_a = runtime
        .load(&wasm, &app_id, "me", host_a.clone())
        .context("cannot load the app for me")?;
    let mut app_b = runtime
        .load(&wasm, &app_id, "bob", host_b.clone())
        .context("cannot load the app for bob")?;
    let mut renderer_a = Renderer::new();
    let mut renderer_b = Renderer::new();
    app_a.start()?;
    app_b.start()?;
    drain(&mut app_a, &mut renderer_a)?;
    drain(&mut app_b, &mut renderer_b)?;

    // Each side acts in turn, syncing in between, so one can act on what the other created.
    run(&mut app_a, &mut renderer_a, a_actions)?;
    converge(
        &host_a,
        &host_b,
        &mut app_a,
        &mut app_b,
        &mut renderer_a,
        &mut renderer_b,
    )?;
    run(&mut app_b, &mut renderer_b, b_actions)?;
    converge(
        &host_a,
        &host_b,
        &mut app_a,
        &mut app_b,
        &mut renderer_a,
        &mut renderer_b,
    )?;

    renderer_a.print("me")?;
    renderer_b.print("bob")?;
    if !renderer_a.same_as(&renderer_b) {
        bail!("the two peers' interfaces differ");
    }
    println!("identical");
    Ok(())
}

fn run(app: &mut App, renderer: &mut Renderer, actions: Vec<render::Action>) -> Result<()> {
    for action in actions {
        if let Some(set) = &action.set {
            renderer.set(set);
        }
        let changed = app.action(
            &action.name,
            action.payload,
            render::from_json(renderer.signals().clone()),
        )?;
        drain(app, renderer)?;
        if changed {
            settle(app, renderer)?;
        }
    }
    Ok(())
}

fn converge(
    a: &Arc<Shared>,
    b: &Arc<Shared>,
    app_a: &mut App,
    app_b: &mut App,
    renderer_a: &mut Renderer,
    renderer_b: &mut Renderer,
) -> Result<()> {
    let mut quiet = 0;
    for _ in 0..100 {
        let synced = sync(a, b)?;
        let wrote_a = settle(app_a, renderer_a)?;
        let wrote_b = settle(app_b, renderer_b)?;
        if synced || wrote_a || wrote_b {
            quiet = 0;
        } else {
            quiet += 1;
            if quiet == 2 {
                return Ok(());
            }
        }
    }
    bail!("the peers did not converge")
}

fn sync(a: &Arc<Shared>, b: &Arc<Shared>) -> Result<bool> {
    let mut changed = false;
    for _ in 0..64 {
        let mut any = false;
        let to_b = a.0.lock().unwrap().sync_message("b");
        if let Some(message) = to_b {
            b.0.lock().unwrap().receive_sync("a", &message)?;
            any = true;
        }
        let to_a = b.0.lock().unwrap().sync_message("a");
        if let Some(message) = to_a {
            a.0.lock().unwrap().receive_sync("b", &message)?;
            any = true;
        }
        if !any {
            break;
        }
        changed = true;
    }
    Ok(changed)
}

fn drain(app: &mut App, renderer: &mut Renderer) -> Result<()> {
    let lines = app.take_ui();
    if !lines.is_empty() {
        renderer.apply(&lines)?;
    }
    Ok(())
}

fn settle(app: &mut App, renderer: &mut Renderer) -> Result<bool> {
    let mut wrote = false;
    for _ in 0..64 {
        let changed = app.notify()?;
        drain(app, renderer)?;
        if changed {
            wrote = true;
        } else {
            return Ok(wrote);
        }
    }
    bail!("the app did not settle")
}
