//! `flipflop-cli` hands its commands to the process that owns the data dir.

use std::{path::PathBuf, process::Command};

use flipflop::{core::Core, ipc};

fn cli(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_flipflop-cli"))
        .args(args)
        .env("FLIPFLOP_DIR", dir)
        .env("FLIPFLOP_DOWNLOADS", dir.join("dl"))
        .current_dir(dir)
        .output()
        .unwrap()
}

fn text(o: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn commands_run_inside_the_owner() {
    let dir: PathBuf = std::env::temp_dir().join(format!("flipflop-ipc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let core = Core::start(&dir, false, |_| {}).await.unwrap();
    ipc::serve(&dir, core.clone()).unwrap();
    assert!(Core::dir_in_use(&dir));

    let out = tokio::task::spawn_blocking({
        let dir = dir.clone();
        move || {
            let id = cli(&dir, &["id"]);
            let new = cli(&dir, &["group", "new"]);
            let list = cli(&dir, &["group", "list"]);
            std::fs::write(dir.join("hello.txt"), "hi").unwrap();
            let gid = text(&new).split_whitespace().next().unwrap().to_owned();
            // relative to the CLI's working directory, not the owner's
            let offer = cli(&dir, &["--wait", "1", "file", "offer", &gid, "hello.txt"]);
            let missing = cli(&dir, &["file", "offer", &gid, "nope.txt"]);
            let bad = cli(&dir, &["group", "show", "zzzz"]);
            let serve = cli(&dir, &["serve", "--secs", "1"]);
            (id, new, list, offer, missing, bad, serve)
        }
    })
    .await
    .unwrap();
    let (id, new, list, offer, missing, bad, serve) = out;

    assert!(id.status.success(), "{}", text(&id));
    assert!(
        text(&id).contains(&core.id().to_string()),
        "ran in the owner: {}",
        text(&id)
    );
    assert!(new.status.success(), "{}", text(&new));
    // the group the CLI made exists in the owner
    assert_eq!(core.groups().groups.len(), 1);
    assert!(
        text(&list).contains(&core.groups().groups[0].id[..8]),
        "{}",
        text(&list)
    );
    assert!(
        offer.status.success() && text(&offer).starts_with("offered"),
        "{}",
        text(&offer)
    );
    assert!(
        !missing.status.success() && text(&missing).contains("nope.txt"),
        "{}",
        text(&missing)
    );
    assert!(
        !bad.status.success() && text(&bad).contains("no group matches"),
        "{}",
        text(&bad)
    );
    assert!(
        serve.status.success() && text(&serve).contains("already serving"),
        "{}",
        text(&serve)
    );

    core.shutdown().await;
    assert!(!Core::dir_in_use(&dir));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn without_an_owner_the_cli_runs_its_own_node() {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("flipflop-ipc-solo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let d = dir.clone();
    let (a, b) = tokio::task::spawn_blocking(move || {
        (cli(&d, &["group", "new"]), cli(&d, &["group", "list"]))
    })
    .await
    .unwrap();
    assert!(a.status.success(), "{}", text(&a));
    assert!(text(&b).contains(&text(&a)[..8]), "{}", text(&b));
    let _ = std::fs::remove_dir_all(&dir);
}
