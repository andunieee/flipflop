//! Runs one of `examples/miniapps`' wasm apps over a throwaway group document, as a renderer would:
//! apply its hypereffect frames, then send it actions.
//!
//! ```text
//! cargo run --example miniapp -- examples/miniapps/dist/checklist.wasm actions.json
//! ```
//!
//! An actions file is a JSON array of `{"name": "...", "payload": {...}, "set": {...}}`. `set` is a
//! merge patch applied to the renderer's signals first, as a user editing a bound input would.

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
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    let mut args = std::env::args().skip(1);
    let wasm_path = args
        .next()
        .context("usage: miniapp <app.wasm> [actions.json ...]")?;
    let inputs: Vec<String> = args.collect();

    let wasm = std::fs::read(&wasm_path).with_context(|| format!("cannot read {wasm_path}"))?;
    let app_id = std::path::Path::new(&wasm_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .context("the wasm file has no stem")?
        .to_owned();

    let host = Arc::new(Shared(Mutex::new(GroupDoc::new())));
    {
        let mut doc = host.0.lock().unwrap();
        for peer in ["me", "bob", "carol"] {
            doc.join(peer, peer);
        }
    }

    let mut app = Runtime::default()
        .load(&wasm, &app_id, "me", host.clone())
        .context("cannot load the app")?;
    let mut renderer = Renderer::new();
    app.start().context("the app failed to start")?;
    drain(&mut app, &mut renderer)?;

    for path in &inputs {
        for action in render::read_actions(path)? {
            if let Some(set) = &action.set {
                renderer.set(set);
            }
            let changed = app.action(
                &action.name,
                action.payload,
                render::from_json(renderer.signals().clone()),
            )?;
            drain(&mut app, &mut renderer)?;
            if changed {
                settle(&mut app, &mut renderer)?;
            }
        }
    }

    renderer.print("final")?;
    Ok(())
}

fn drain(app: &mut App, renderer: &mut Renderer) -> Result<()> {
    let lines = app.take_ui();
    if !lines.is_empty() {
        renderer.apply(&lines)?;
    }
    Ok(())
}

fn settle(app: &mut App, renderer: &mut Renderer) -> Result<()> {
    for _ in 0..64 {
        let wrote = app.notify()?;
        drain(app, renderer)?;
        if !wrote {
            return Ok(());
        }
    }
    bail!("the app did not settle")
}
