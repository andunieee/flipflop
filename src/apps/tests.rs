use std::sync::Mutex;

use super::*;
use codec::{TAG_COUNTER, TAG_TEXT};

struct Shared(Mutex<GroupDoc>);

impl DocHost for Shared {
    fn with_doc(&self, f: &mut dyn FnMut(&mut GroupDoc)) {
        f(&mut self.0.lock().unwrap())
    }
}

fn shared() -> Arc<Shared> {
    Arc::new(Shared(Mutex::new(GroupDoc::new())))
}

fn sync(a: &mut GroupDoc, b: &mut GroupDoc) {
    for _ in 0..20 {
        let mut quiet = true;
        if let Some(m) = a.sync_message("b") {
            b.receive_sync("a", &m).unwrap();
            quiet = false;
        }
        if let Some(m) = b.sync_message("a") {
            a.receive_sync("b", &m).unwrap();
            quiet = false;
        }
        if quiet {
            return;
        }
    }
    panic!("sync did not settle");
}

const LOG: usize = 0x10000;

/// An app that, on its first event, makes the requests in `script`, and logs every event and
/// response to its memory: `[u32 size of the log][u32 len, bytes]…` at [`LOG`].
fn scripted(script: &[Cbor]) -> Vec<u8> {
    let mut data = (script.len() as u32).to_le_bytes().to_vec();
    for request in script {
        let bytes = codec::encode(request);
        data.extend((bytes.len() as u32).to_le_bytes());
        data.extend(bytes);
    }
    let data: String = data.iter().map(|b| format!("\\{b:02x}")).collect();
    wat::parse_str(format!(
        r#"(module
  (import "flipflop" "call" (func $call (param i32 i32) (result i32)))
  (import "flipflop" "take" (func $take (param i32)))
  (memory (export "memory") 4)
  (data (i32.const 0) "{data}")
  (global $log (mut i32) (i32.const 0x10004))
  (global $heap (mut i32) (i32.const 0x30000))
  (global $ran (mut i32) (i32.const 0))
  (func $alloc (export "ff_alloc") (param $len i32) (result i32)
    (local $p i32)
    (local.set $p (global.get $heap))
    (global.set $heap (i32.add (global.get $heap) (local.get $len)))
    (local.get $p))
  (func $log (param $ptr i32) (param $len i32)
    (i32.store (global.get $log) (local.get $len))
    (memory.copy (i32.add (global.get $log) (i32.const 4)) (local.get $ptr) (local.get $len))
    (global.set $log (i32.add (global.get $log) (i32.add (i32.const 4) (local.get $len))))
    (i32.store (i32.const 0x10000) (i32.sub (global.get $log) (i32.const 0x10004))))
  (func (export "ff_event") (param $ptr i32) (param $len i32)
    (local $n i32) (local $at i32) (local $rlen i32) (local $resp i32)
    (call $log (local.get $ptr) (local.get $len))
    (if (global.get $ran) (then (return)))
    (global.set $ran (i32.const 1))
    (local.set $n (i32.load (i32.const 0)))
    (local.set $at (i32.const 4))
    (block $done (loop $next
      (br_if $done (i32.eqz (local.get $n)))
      (local.set $rlen (call $call (i32.add (local.get $at) (i32.const 4)) (i32.load (local.get $at))))
      (local.set $resp (call $alloc (local.get $rlen)))
      (call $take (local.get $resp))
      (call $log (local.get $resp) (local.get $rlen))
      (local.set $at (i32.add (local.get $at) (i32.add (i32.const 4) (i32.load (local.get $at)))))
      (local.set $n (i32.sub (local.get $n) (i32.const 1)))
      (br $next))))
)"#
    ))
    .unwrap()
}

/// Everything the app logged, decoded.
fn log(app: &App) -> Vec<Cbor> {
    let mem = app.memory.data(&app.store);
    let size = u32::from_le_bytes(mem[LOG..LOG + 4].try_into().unwrap()) as usize;
    let (mut at, mut out) = (LOG + 4, Vec::new());
    while at < LOG + 4 + size {
        let len = u32::from_le_bytes(mem[at..at + 4].try_into().unwrap()) as usize;
        out.push(ciborium::from_reader(&mem[at + 4..at + 4 + len]).unwrap());
        at += 4 + len;
    }
    out
}

fn path(steps: &[&str]) -> Cbor {
    Cbor::Array(
        steps
            .iter()
            .map(|s| match s.parse::<u64>() {
                Ok(i) => Cbor::Integer(i.into()),
                Err(_) => text(*s),
            })
            .collect(),
    )
}

fn op(op: &'static str, p: &[&str], extra: Vec<(&'static str, Cbor)>) -> Cbor {
    map([("op", text(op)), ("path", path(p))]
        .into_iter()
        .chain(extra))
}

fn field<'a>(v: &'a Cbor, name: &str) -> &'a Cbor {
    v.as_map()
        .unwrap()
        .iter()
        .find(|(k, _)| k.as_text() == Some(name))
        .map(|(_, v)| v)
        .unwrap_or_else(|| panic!("no {name} in {v:?}"))
}

fn tagged(tag: u64, v: Cbor) -> Cbor {
    Cbor::Tag(tag, Box::new(v))
}

#[test]
fn an_app_writes_reads_and_hears_of_changes() {
    let host = shared();
    let wasm = scripted(&[
        op("put", &["title"], vec![("value", text("chess"))]),
        op(
            "put",
            &["board"],
            vec![(
                "value",
                map([
                    ("moves", Cbor::Array(vec![])),
                    ("notes", tagged(TAG_TEXT, text("hi"))),
                ]),
            )],
        ),
        op(
            "insert",
            &["board", "moves", "0"],
            vec![("value", text("e4"))],
        ),
        op(
            "put",
            &["score"],
            vec![("value", tagged(TAG_COUNTER, Cbor::Integer(0.into())))],
        ),
        op(
            "increment",
            &["score"],
            vec![("by", Cbor::Integer(2.into()))],
        ),
        op("get", &["board"], vec![]),
        op(
            "put",
            &["members", "x"],
            vec![("root", text("group")), ("value", Cbor::Null)],
        ),
        op("watch", &["members"], vec![]),
    ]);
    let mut app = Runtime::default()
        .load(&wasm, "chess", "me", host.clone())
        .unwrap();
    assert!(app.start().unwrap());

    let log = log(&app);
    assert_eq!(field(&log[0], "type"), &text("start"));
    assert!(log[1..6].iter().all(|r| field(r, "ok") == &Cbor::Null));
    assert_eq!(
        field(&log[6], "ok"),
        &map([
            ("moves", Cbor::Array(vec![text("e4")])),
            ("notes", tagged(TAG_TEXT, text("hi"))),
        ])
    );
    assert!(
        field(&log[7], "err")
            .as_text()
            .unwrap()
            .contains("own section")
    );

    // Its own writes come back to it.
    assert!(!app.notify().unwrap());
    let event = self::log(&app).pop().unwrap();
    assert_eq!(field(&event, "type"), &text("change"));
    let own = field(&event, "patches").as_array().unwrap().clone();
    assert!(own.contains(&map([
        ("root", text("app")),
        ("path", path(&["title"])),
        ("action", text("put")),
        ("value", text("chess")),
    ])));

    // Another member moves and joins; the app hears of both.
    let mut other = GroupDoc::new();
    {
        let mut doc = host.0.lock().unwrap();
        sync(&mut doc, &mut other);
        other.join("bob", "Bob's laptop");
        let section = other.app_section("chess").unwrap();
        let moves = ["board", "moves"].map(|k| Prop::Map(k.into()));
        let moves = codec::resolve(other.raw(), &section, &moves).unwrap();
        codec::insert(other.raw(), &moves, 1, text("e5")).unwrap();
        sync(&mut doc, &mut other);
    }
    assert!(!app.notify().unwrap());
    let event = self::log(&app).pop().unwrap();
    let mut patches = field(&event, "patches").as_array().unwrap().clone();
    patches.sort_by_key(|p| format!("{p:?}"));
    assert_eq!(
        patches,
        [
            map([
                ("root", text("app")),
                ("path", path(&["board", "moves", "1"])),
                ("action", text("insert")),
                ("values", Cbor::Array(vec![text("e5")])),
            ]),
            map([
                ("root", text("group")),
                ("path", path(&["members", "bob"])),
                ("action", text("put")),
                ("value", text("Bob's laptop")),
            ]),
        ]
    );

    // Nothing new: no event.
    let before = self::log(&app).len();
    assert!(!app.notify().unwrap());
    assert_eq!(self::log(&app).len(), before);
}

#[test]
fn an_app_that_runs_forever_is_stopped() {
    let wasm = wat::parse_str(
        r#"(module
  (memory (export "memory") 1)
  (func (export "ff_alloc") (param i32) (result i32) (i32.const 0))
  (func (export "ff_event") (param i32 i32) (loop (br 0))))"#,
    )
    .unwrap();
    let mut app = Runtime::default()
        .load(&wasm, "spin", "me", shared())
        .unwrap();
    assert!(app.start().is_err());
    assert!(app.notify().is_err());
}
