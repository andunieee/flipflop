//! A date (and time) picker made of selects: day, month and year, then hour and minute. The signals
//! live under `form.<path>` as strings: `day`, `month`, `year`, `hour`, `min`.

use std::collections::HashSet;
use std::sync::Mutex;

use ciborium::Value as Cbor;

use crate::act::Act;
use crate::time;
use crate::ui::{self, button, row, El, Page};
use crate::value::{map, text};
use crate::Host;

pub fn options(pairs: &[(String, String)]) -> Cbor {
    Cbor::Array(
        pairs
            .iter()
            .map(|(v, l)| map([("value", text(v)), ("label", text(l))]))
            .collect(),
    )
}

fn same(values: impl IntoIterator<Item = String>) -> Vec<(String, String)> {
    values.into_iter().map(|v| (v.clone(), v)).collect()
}

/// The signals for a picker showing `day` at `minute`.
pub fn value(day: i64, minute: i64) -> Cbor {
    let (y, m, d) = time::ymd(day);
    map([
        ("day", text(d.to_string())),
        ("month", text(m.to_string())),
        ("year", text(y.to_string())),
        ("hour", text(format!("{:02}", minute / 60))),
        ("min", text(format!("{:02}", minute / 5 * 5 % 60))),
    ])
}

/// What a picker starts at: tomorrow evening.
pub fn initial() -> Cbor {
    value(time::today() + 1, 18 * 60)
}

/// Pickers that have had their initial value sent, by their path.
static READY: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Give the picker at `form.<path>` (a `.`-joined path) its initial value once per session, for a
/// picker that appears after the session started. `initial_form` covers those there from the start.
pub fn ensure(host: &Host, path: &[&str]) {
    if mark(path) {
        ui::signal(host, map([("form", nest(path, initial()))]));
    }
}

/// `value` as the signal at `path`: `{a: {b: value}}`.
pub fn nest(path: &[&str], value: Cbor) -> Cbor {
    path.iter()
        .rev()
        .fold(value, |inner, step| Cbor::Map(vec![(text(*step), inner)]))
}

/// Note that the picker at `path` has its initial value; true if it was not noted before.
pub fn mark(path: &[&str]) -> bool {
    READY
        .lock()
        .unwrap()
        .get_or_insert_with(HashSet::new)
        .insert(path.join("."))
}

pub fn reset() {
    *READY.lock().unwrap() = None;
}

/// A picker: `id` is its element id prefix, `path` where its signals are, below `form`.
pub fn add(page: &mut Page, id: &str, parent: &str, path: &[&str], with_time: bool) {
    let bind = |part: &str| format!("form.{}.{part}", path.join("."));
    let this_year = time::ymd(time::today()).0;
    let select = |part: &str, label: &str, pairs: Vec<(String, String)>| {
        El::new(&format!("{id}-{part}"), "select")
            .parent(&format!("{id}-date"))
            .label(label)
            .bind(&bind(part))
            .set("options", options(&pairs))
    };
    page.add(row(&format!("{id}-date"), parent));
    page.add(select("day", "Day", same((1..=31).map(|d| d.to_string()))));
    page.add(select(
        "month",
        "Month",
        (1..=12)
            .map(|m| (m.to_string(), time::month_name(m).to_owned()))
            .collect(),
    ));
    page.add(select(
        "year",
        "Year",
        same((this_year..this_year + 3).map(|y| y.to_string())),
    ));
    if with_time {
        page.add(row(&format!("{id}-at"), parent));
        let at = |part: &str, label: &str, pairs: Vec<(String, String)>| {
            El::new(&format!("{id}-{part}"), "select")
                .parent(&format!("{id}-at"))
                .label(label)
                .bind(&bind(part))
                .set("options", options(&pairs))
        };
        page.add(at("hour", "Hour", same((0..24).map(|h| format!("{h:02}")))));
        page.add(at(
            "min",
            "Minute",
            same((0..12).map(|m| format!("{:02}", m * 5))),
        ));
    }
}

/// Quick buttons that set the picker at `path` to today or tomorrow (`pick_day {path, day}`).
pub fn quick_buttons(page: &mut Page, id: &str, parent: &str, name: &str) {
    page.add(row(id, parent));
    for (label, day) in [("Today", time::today()), ("Tomorrow", time::today() + 1)] {
        page.add(button(
            &format!("{id}-{label}"),
            id,
            "plain",
            label,
            "pick_day",
            crate::act::payload(&[("path", name), ("day", &day.to_string())]),
        ));
    }
}

/// The signals `pick_day` answers with: the picker at `path` set to `day`.
pub fn pick_patch(act: &Act) -> Option<Cbor> {
    let day: i64 = act.arg("day").parse().ok()?;
    let mut patch = value(day, 0);
    if let Cbor::Map(entries) = &mut patch {
        // Only the date: the time of day stays as it is.
        entries.retain(|(k, _)| matches!(k.as_text(), Some("day" | "month" | "year")));
    }
    let path: Vec<&str> = act.arg("path").split('.').collect();
    Some(map([("form", nest(&path, patch))]))
}

/// The moment a picker shows (UTC milliseconds), or why it is not one.
pub fn read(act: &Act, path: &[&str], with_time: bool) -> Result<i64, String> {
    let get = |part: &str| -> Option<i64> {
        let mut full = vec!["form"];
        full.extend_from_slice(path);
        full.push(part);
        act.signal(&full)?.as_text()?.parse().ok()
    };
    let (Some(d), Some(m), Some(y)) = (get("day"), get("month"), get("year")) else {
        return Err("Choose a day.".into());
    };
    let day = time::day_from_ymd(y, m, d)
        .ok_or_else(|| format!("There is no {d} {} {y}.", time::month_name(m)))?;
    let minute = if with_time {
        get("hour").unwrap_or(0) * 60 + get("min").unwrap_or(0)
    } else {
        0
    };
    Ok(time::moment(day, minute))
}
