//! Experimental wasm apps for flipflop's app runtime (`src/apps.rs`).
//!
//! Each app is this crate built with one feature, so one `.wasm` per app (`build.sh`). The module
//! talks CBOR with the host and is a hypereffect *server*: the group document is its state, and it
//! sends hypereffect messages that the local renderer draws.
//!
//! # Exports
//!
//! - `ff_get_params`: before the app is added to a group: the name of its tab and the questions to
//!   ask the user (see `params` below).
//! - `ff_start`: first, once. Carries the user's answers (`params`). Send the metadata, the initial
//!   signals and the element tree.
//! - `ff_doc_change`: the document changed, locally or from a syncing peer (the event carries the
//!   patches). Re-send what the interface needs.
//! - `ff_action`: the local renderer sent an action (name, payload, signals). Validate it, write the
//!   document, and render; if it is refused, say why in a notice. The following `ff_doc_change`
//!   renders the same result on every other device.
//!
//! Every event carries the device's clock and time zone (see [`time`]), so dates are typed and shown
//! in local time while the document holds UTC milliseconds.
//!
//! # An app
//!
//! A module in `apps/` with `NAME`, `params()` (what `ff_get_params` tells), `form(host)` (the user's initial input signals), `page(host)` (the
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

static PARAMS: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());

/// What the user answered when adding the app, for the param `key` (empty if there is none).
pub fn param(key: &str) -> String {
    let params = PARAMS.lock().unwrap();
    let found = params.iter().find(|(k, _)| k == key);
    found.map(|(_, v)| v.clone()).unwrap_or_default()
}

/// One question of the form shown when adding the app.
pub fn question(key: &str, label: &str, default: &str, placeholder: &str) -> Cbor {
    value::map([
        ("key", value::text(key)),
        ("label", value::text(label)),
        ("default", value::text(default)),
        ("placeholder", value::text(placeholder)),
    ])
}

/// What `ff_get_params` tells: the tab's name (as its default and its placeholder) and the
/// questions.
pub fn params(name: &str, placeholder: &str, questions: Vec<Cbor>) -> Cbor {
    value::map([
        (
            "name",
            value::map([
                ("default", value::text(name)),
                ("placeholder", value::text(placeholder)),
            ]),
        ),
        ("params", Cbor::Array(questions)),
    ])
}

/// What the app takes when it is added: the CBOR is left in memory for the host, whose address and
/// length come back packed (address in the high 32 bits).
#[no_mangle]
pub extern "C" fn ff_get_params() -> i64 {
    let mut bytes = Vec::new();
    let _ = ciborium::into_writer(&apps::current::params(), &mut bytes);
    let packed = ((bytes.as_ptr() as u64) << 32) | bytes.len() as u64;
    std::mem::forget(bytes);
    packed as i64
}

/// Read the event the host left at `ptr`, and take the clock and who we are from it.
fn read_event(ptr: i32, len: i32) -> Option<Cbor> {
    let len = len.max(0) as usize;
    // The host allocated this with `ff_alloc`, so its capacity is `len`.
    let bytes = unsafe { Vec::from_raw_parts(ptr as *mut u8, len, len) };
    let event = ciborium::from_reader::<Cbor, _>(bytes.as_slice()).ok()?;
    time::set_clock(
        value::i64_of(&event, "ts").unwrap_or(0),
        value::i64_of(&event, "tz").unwrap_or(0),
    );
    if let Some(me) = str_of(&event, "me") {
        *ME.lock().unwrap() = me.to_owned();
    }
    Some(event)
}

/// The first event, once.
#[no_mangle]
pub extern "C" fn ff_start(ptr: i32, len: i32) {
    let Some(event) = read_event(ptr, len) else {
        return;
    };
    let host = Host;
    use apps::current as app;
    *PARAMS.lock().unwrap() = value::text_entries(
        &value::field(&event, "params")
            .cloned()
            .unwrap_or(Cbor::Null),
    )
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.as_text().unwrap_or("").to_owned()))
    .collect();
    ui::reset();
    picker::reset();
    // Member names, for apps that show or default to people.
    host.watch(&[key("members")]);
    ui::start(&host, &app::title(), app::form(&host), app::page(&host));
}

/// The document changed.
#[no_mangle]
pub extern "C" fn ff_doc_change(ptr: i32, len: i32) {
    if read_event(ptr, len).is_some() {
        let host = Host;
        ui::update(&host, apps::current::page(&host));
    }
}

/// The local renderer sent an action.
#[no_mangle]
pub extern "C" fn ff_action(ptr: i32, len: i32) {
    let Some(event) = read_event(ptr, len) else {
        return;
    };
    let host = Host;
    use apps::current as app;
    ui::clear_error();
    ui::begin_action();
    if let Err(why) = app::action(&host, &event) {
        ui::fail(why);
    }
    ui::refresh(&host, app::page(&host));
}
