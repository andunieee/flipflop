//! Experimental wasm apps for flipflop's app runtime (`src/apps.rs`).
//!
//! Each app is this crate built with one feature, so one `.wasm` per app (`build.sh`). The module
//! talks CBOR with the host and is a hypereffect *server*: the group document is its state, and it
//! sends hypereffect messages that the local renderer draws.
//!
//! # Events
//!
//! - `start`: first, once. Send the metadata, the initial signals and the element tree.
//! - `change`: the document changed, locally or from a syncing peer (the event carries the
//!   patches). Re-send what the interface needs.
//! - `action`: the local renderer sent an action (name, payload, signals). Validate it, write the
//!   document, and render; if it is refused, say why in a notice. The following `change` renders
//!   the same result on every other device.
//!
//! Every event carries the device's clock and time zone (see [`time`]), so dates are typed and shown
//! in local time while the document holds UTC milliseconds.
//!
//! # An app
//!
//! A module in `apps/` with `NAME`, `form(host)` (the user's initial input signals), `page(host)` (the
//! whole interface for the document as it is) and `action(host, event)` (`Err` is shown to the user).
//! The driver below does the rest: it renders at the start, after every change and after every
//! action, and the renderer only receives what differs from the last frame.
//!
//! # Why every value is a scalar under its own key
//!
//! `put` of a map or a list makes a new Automerge object. Two devices doing that at the same key
//! make two objects, and a merge keeps one: everything written inside the other is lost. An app runs
//! on every device, so it would do exactly that. State is therefore flat — `list<US>groceries<US>name`,
//! `item<US>groceries<US>milk<US>done` — and every value is a scalar string, integer, boolean or
//! null. Different keys merge; the same key is last-writer-wins.
//!
//! `<US>` is U+001F, unlikely in an id.

#![allow(dead_code)] // each build has one app, so the shared helpers are not all used

mod act;
mod apps;
mod doc;
mod host;
mod picker;
mod time;
mod ui;
mod value;

use ciborium::Value as Cbor;

use host::Host;
use value::{key, str_of};

static ME: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// Our own peer id, from the start event.
pub fn me() -> String {
    ME.lock().unwrap().clone()
}

/// The separator in flat keys: U+001F.
pub const SEP: char = '\u{1f}';

/// A flat key from its parts.
pub fn join(parts: &[&str]) -> String {
    parts.join(&SEP.to_string())
}

/// The prefix shared by every key of one thing (`a<US>x`), trailing separator included.
pub fn prefix(parts: &[&str]) -> String {
    let mut s = join(parts);
    s.push(SEP);
    s
}

/// An id for something an action creates: unique to this device and action.
pub fn new_id(event: &Cbor) -> String {
    format!(
        "{}-{}-{}",
        value::str_of(event, "me").unwrap_or("me"),
        value::i64_of(event, "ts").unwrap_or(0),
        value::i64_of(event, "seq").unwrap_or(0)
    )
}

pub fn event_me(event: &Cbor) -> String {
    value::str_of(event, "me").unwrap_or("").to_owned()
}

pub fn event_ts(event: &Cbor) -> i64 {
    value::i64_of(event, "ts").unwrap_or(0)
}

pub fn payload(event: &Cbor) -> Cbor {
    value::field(event, "payload")
        .cloned()
        .unwrap_or(Cbor::Null)
}

pub fn signals(event: &Cbor) -> Cbor {
    value::field(event, "signals")
        .cloned()
        .unwrap_or(Cbor::Null)
}

pub fn form(signals: &Cbor) -> Option<&Cbor> {
    value::field(signals, "form")
}

pub fn form_str<'a>(signals: &'a Cbor, key: &str) -> Option<&'a str> {
    form(signals).and_then(|f| value::str_of(f, key))
}

pub fn form_i64(signals: &Cbor, key: &str) -> Option<i64> {
    form(signals).and_then(|f| value::i64_of(f, key))
}

/// A comma-separated list, trimmed, without empties.
pub fn words(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Room for one event, which the host fills and the app frees.
#[no_mangle]
pub extern "C" fn ff_alloc(len: i32) -> i32 {
    let len = len.max(0) as usize;
    let mut buffer = Vec::<u8>::with_capacity(len);
    let ptr = buffer.as_mut_ptr();
    std::mem::forget(buffer);
    ptr as i32
}

/// One event from the host: `start`, `change` or `action`.
#[no_mangle]
pub extern "C" fn ff_event(ptr: i32, len: i32) {
    let len = len.max(0) as usize;
    // The host allocated this with `ff_alloc`, so its capacity is `len`.
    let bytes = unsafe { Vec::from_raw_parts(ptr as *mut u8, len, len) };
    let Ok(event) = ciborium::from_reader::<Cbor, _>(bytes.as_slice()) else {
        return;
    };
    let host = Host;
    time::set_clock(
        value::i64_of(&event, "ts").unwrap_or(0),
        value::i64_of(&event, "tz").unwrap_or(0),
    );
    if let Some(me) = str_of(&event, "me") {
        *ME.lock().unwrap() = me.to_owned();
    }
    use apps::current as app;
    match str_of(&event, "type") {
        Some("start") => {
            ui::reset();
            picker::reset();
            // Member names, for apps that show or default to people.
            host.watch(&[key("members")]);
            ui::start(&host, app::NAME, app::form(&host), app::page(&host));
        }
        Some("change") => ui::update(&host, app::page(&host)),
        Some("action") => {
            ui::clear_error();
            ui::begin_action();
            if let Err(why) = app::action(&host, &event) {
                ui::fail(why);
            }
            ui::refresh(&host, app::page(&host));
        }
        _ => {}
    }
}
