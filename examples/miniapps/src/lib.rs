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
//!   document, and render. The following `change` renders the same result on every other device.
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

mod apps;
mod host;
mod ui;
mod value;

use ciborium::Value as Cbor;

use host::Host;
use value::{key, str_of};

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

/// `YYYY-MM-DD HH:MM` from epoch milliseconds (UTC), without a date library.
pub fn fmt_time(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        sod / 3600,
        (sod % 3600) / 60
    )
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    (y + i64::from(m <= 2), m, d)
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
    match str_of(&event, "type") {
        Some("start") => {
            ui::reset();
            // Member names, for apps that show or default to people.
            host.watch(&[key("members")]);
            start(&host);
        }
        Some("change") => change(&host),
        Some("action") => action(&host, &event),
        _ => {}
    }
}

fn start(host: &Host) {
    #[cfg(feature = "checklist")]
    apps::checklist::start(host);
    #[cfg(feature = "expenses")]
    apps::expenses::start(host);
    #[cfg(feature = "poll")]
    apps::poll::start(host);
    #[cfg(feature = "availability")]
    apps::availability::start(host);
    #[cfg(feature = "calendar")]
    apps::calendar::start(host);
}

fn change(host: &Host) {
    #[cfg(feature = "checklist")]
    apps::checklist::change(host);
    #[cfg(feature = "expenses")]
    apps::expenses::change(host);
    #[cfg(feature = "poll")]
    apps::poll::change(host);
    #[cfg(feature = "availability")]
    apps::availability::change(host);
    #[cfg(feature = "calendar")]
    apps::calendar::change(host);
}

fn action(host: &Host, event: &Cbor) {
    #[cfg(feature = "checklist")]
    apps::checklist::action(host, event);
    #[cfg(feature = "expenses")]
    apps::expenses::action(host, event);
    #[cfg(feature = "poll")]
    apps::poll::action(host, event);
    #[cfg(feature = "availability")]
    apps::availability::action(host, event);
    #[cfg(feature = "calendar")]
    apps::calendar::action(host, event);
}
