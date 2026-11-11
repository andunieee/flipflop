//! Renders the app's pages to PNGs without opening a window, over a fresh node in a temporary
//! directory: `cargo run -p flipflop-app --example snapshot -- <out dir>` (with
//! `FLIPFLOP_PHONE_LAYOUT=1` for the phone layout).

use std::time::Duration;

use flipflop_app::{Backend, app};
use freya::prelude::*;
use freya_testing::prelude::*;

fn main() {
    let out = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "snapshots".into()));
    std::fs::create_dir_all(&out).unwrap();
    let dir = std::env::temp_dir().join(format!("flipflop-snapshot-{}", std::process::id()));
    let phone = std::env::var_os("FLIPFLOP_PHONE_LAYOUT").is_some_and(|v| v == "1");
    let size = if phone { (412., 860.) } else { (1100., 720.) };

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let _guard = rt.enter();
    let backend = Backend::start(rt.handle().clone(), dir.clone());
    let (mut runner, ()) = TestingRunner::new(
        app,
        size.into(),
        |r| {
            r.provide_root_context(|| backend.clone());
        },
        1.0,
    );

    let settle = |runner: &mut TestingRunner, ms: u64| {
        for _ in 0..(ms / 50).max(1) {
            std::thread::sleep(Duration::from_millis(50));
            runner.poll(Duration::from_millis(16), Duration::from_millis(50));
        }
        runner.sync_and_update();
    };
    // The node is up once it has a ticket to show.
    for _ in 0..200 {
        settle(&mut runner, 50);
        if backend.core().is_some_and(|c| !c.ticket().is_empty()) {
            break;
        }
    }
    settle(&mut runner, 500);

    let click = |runner: &mut TestingRunner, text: &str| {
        // The sidebar comes before the page, the bottom navigation after it.
        let found = runner.find_many(|node, element| {
            let label = Label::try_downcast(element)?;
            (label.text == text).then(|| node.layout().area.center())
        });
        let at = if phone { found.last() } else { found.first() }.copied();
        match at {
            Some(at) => {
                runner.click_cursor((at.x as f64, at.y as f64));
                settle(runner, 300);
            }
            None => eprintln!("no {text:?} to click"),
        }
    };
    let shot = |runner: &mut TestingRunner, name: &str| {
        runner.render_to_file(out.join(format!("{name}.png")));
    };

    shot(&mut runner, "home");
    let nav = |label: &str| label.to_owned();
    for (label, name) in [(nav("Peers"), "peers"), (nav(if phone { "Peers" } else { "Add a peer" }), "add"), (nav("Settings"), "settings")] {
        if phone && name == "add" {
            click(&mut runner, "Add");
        } else {
            click(&mut runner, &label);
        }
        shot(&mut runner, name);
    }
    if phone {
        click(&mut runner, "Groups");
        shot(&mut runner, "groups");
        click(&mut runner, "Guest link");
    } else {
        click(&mut runner, "Guest link");
    }
    settle(&mut runner, 1500);
    shot(&mut runner, "group-info");
    for (label, name) in [("Files", "group-files"), ("Texts", "group-texts"), ("Scratchpad", "group-pad")] {
        click(&mut runner, label);
        shot(&mut runner, name);
    }
    drop(runner);
    backend.shutdown();
    let _ = std::fs::remove_dir_all(dir);
}
