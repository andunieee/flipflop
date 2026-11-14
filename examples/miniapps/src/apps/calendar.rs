//! A shared calendar: events with a start and an end, and who is coming.
//!
//! Flat state:
//! - `c<US><event><US>title|start|end|all_day|place|note|by|ts` (epoch ms)
//! - `r<US><event><US><peer>` = `"yes"`, `"maybe"` or `"no"`
//!
//! Actions:
//! - `{name:"add_event", payload:{title, start, end, place?, note?, all_day?}}`
//! - `{name:"rsvp", payload:{event, value}}`
//! - `{name:"remove_event", payload:{event}}`

use std::collections::BTreeMap;

use ciborium::Value as Cbor;

use crate::ui::{self, El};
use crate::value::{bool_of, i64_of, key, str_of, text};
use crate::Host;

pub fn start(host: &Host) {
    render(host, true);
}

pub fn change(host: &Host) {
    render(host, false);
}

pub fn action(host: &Host, event: &Cbor) {
    let name = str_of(event, "name").unwrap_or("");
    let payload = crate::payload(event);
    let signals = crate::signals(event);
    let id = crate::new_id(event);
    let me = crate::event_me(event);
    let ts = crate::event_ts(event);

    match name {
        "add_event" => {
            let title = str_of(&payload, "title")
                .or_else(|| crate::form_str(&signals, "title"))
                .unwrap_or("")
                .trim()
                .to_owned();
            let start = i64_of(&payload, "start")
                .or_else(|| crate::form_i64(&signals, "start"))
                .unwrap_or(0);
            if title.is_empty() || start <= 0 {
                ui::log(host, "An event needs a title and a start", "warning");
                return;
            }
            let end = i64_of(&payload, "end")
                .or_else(|| crate::form_i64(&signals, "end"))
                .filter(|end| *end >= start)
                .unwrap_or(start + 3_600_000);
            for (field, value) in [
                ("title", text(&title)),
                ("start", crate::value::int(start)),
                ("end", crate::value::int(end)),
                (
                    "all_day",
                    Cbor::Bool(bool_of(&payload, "all_day").unwrap_or(false)),
                ),
                (
                    "place",
                    text(
                        str_of(&payload, "place")
                            .or_else(|| crate::form_str(&signals, "place"))
                            .unwrap_or(""),
                    ),
                ),
                (
                    "note",
                    text(
                        str_of(&payload, "note")
                            .or_else(|| crate::form_str(&signals, "note"))
                            .unwrap_or(""),
                    ),
                ),
                ("by", text(&me)),
                ("ts", crate::value::int(ts)),
            ] {
                host.put(&[key(&crate::join(&["c", &id, field]))], value);
            }
            ui::signal(
                host,
                Cbor::Map(vec![(
                    text("form"),
                    Cbor::Map(vec![
                        (text("title"), text("")),
                        (text("start"), Cbor::Null),
                        (text("end"), Cbor::Null),
                        (text("place"), text("")),
                        (text("note"), text("")),
                    ]),
                )]),
            );
        }
        "rsvp" => {
            let (Some(event), Some(value)) = (
                resolve_event(host, str_of(&payload, "event").unwrap_or("")),
                str_of(&payload, "value"),
            ) else {
                return;
            };
            if matches!(value, "yes" | "maybe" | "no") {
                host.put(&[key(&crate::join(&["r", &event, &me]))], text(value));
            }
        }
        "remove_event" => {
            if let Some(event) = resolve_event(host, str_of(&payload, "event").unwrap_or("")) {
                delete_prefix(host, &crate::prefix(&["c", &event]));
                delete_prefix(host, &crate::prefix(&["r", &event]));
            }
        }
        _ => return,
    }
    // The document write makes the host call us back with `change`, which renders.
}

fn render(host: &Host, initial: bool) {
    let events = state(host);
    let mut el = Vec::new();
    el.push(El::new("cal", "column").set("gap", text("medium")));
    el.push(
        El::new("cal-title", "text")
            .parent("cal")
            .variant("h1")
            .text("Calendar"),
    );
    el.push(El::new("cal-form", "card").parent("cal").title("New event"));
    el.push(
        El::new("cal-name", "input")
            .parent("cal-form")
            .set("label", text("Title"))
            .bind("form.title"),
    );
    el.push(El::new("cal-when", "row").parent("cal-form"));
    el.push(
        El::new("cal-start", "input")
            .parent("cal-when")
            .set("label", text("Start (epoch ms)"))
            .variant("number")
            .bind("form.start"),
    );
    el.push(
        El::new("cal-end", "input")
            .parent("cal-when")
            .set("label", text("End (epoch ms)"))
            .variant("number")
            .bind("form.end"),
    );
    el.push(
        El::new("cal-place", "input")
            .parent("cal-form")
            .set("label", text("Place"))
            .bind("form.place"),
    );
    el.push(
        El::new("cal-note", "input")
            .parent("cal-form")
            .set("label", text("Note"))
            .bind("form.note"),
    );
    el.push(
        El::new("cal-add", "button")
            .parent("cal-form")
            .variant("primary")
            .label("Add event")
            .on_press("add_event", Cbor::Null),
    );

    el.push(
        El::new("cal-list", "column")
            .parent("cal")
            .set("gap", text("small")),
    );
    for event in &events {
        let card = format!("ev-{}", ui::path_id(&event.id));
        el.push(
            El::new(&card, "card")
                .parent("cal-list")
                .title(&event.title),
        );
        el.push(
            El::new(&format!("{card}-when"), "text")
                .parent(&card)
                .variant("caption")
                .text(&format!(
                    "{} – {}",
                    crate::fmt_time(event.start),
                    crate::fmt_time(event.end)
                )),
        );
        if !event.place.is_empty() {
            el.push(
                El::new(&format!("{card}-place"), "text")
                    .parent(&card)
                    .text(&event.place),
            );
        }
        if !event.note.is_empty() {
            el.push(
                El::new(&format!("{card}-note"), "text")
                    .parent(&card)
                    .variant("caption")
                    .text(&event.note),
            );
        }
        el.push(El::new(&format!("{card}-rsvp"), "row").parent(&card));
        for (value, label, variant) in [
            ("yes", "Yes", "success"),
            ("maybe", "Maybe", "warning"),
            ("no", "No", "failure"),
        ] {
            el.push(
                El::new(&format!("{card}-{value}"), "button")
                    .parent(&format!("{card}-rsvp"))
                    .variant(variant)
                    .label(label)
                    .on_press(
                        "rsvp",
                        Cbor::Map(vec![
                            (text("event"), text(&event.id)),
                            (text("value"), text(value)),
                        ]),
                    ),
            );
        }
        el.push(
            El::new(&format!("{card}-who"), "text")
                .parent(&card)
                .variant("caption")
                .text(&format!(
                    "yes: {} · maybe: {} · no: {}",
                    join(&event.yes),
                    join(&event.maybe),
                    join(&event.no)
                )),
        );
        el.push(
            El::new(&format!("{card}-del"), "button")
                .parent(&card)
                .variant("plain")
                .label("Remove")
                .on_press(
                    "remove_event",
                    Cbor::Map(vec![(text("event"), text(&event.id))]),
                ),
        );
    }

    if initial {
        ui::start(
            host,
            "Calendar",
            Cbor::Map(vec![(
                text("form"),
                Cbor::Map(vec![
                    (text("title"), text("")),
                    (text("place"), text("")),
                    (text("note"), text("")),
                ]),
            )]),
            el,
        );
    } else {
        ui::update(host, el);
    }
}

fn join(people: &[String]) -> String {
    if people.is_empty() {
        "—".to_owned()
    } else {
        people.join(", ")
    }
}

struct Event {
    id: String,
    title: String,
    start: i64,
    end: i64,
    place: String,
    note: String,
    yes: Vec<String>,
    maybe: Vec<String>,
    no: Vec<String>,
}

fn state(host: &Host) -> Vec<Event> {
    let mut events: BTreeMap<String, Event> = BTreeMap::new();
    let mut answers: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for flat in host.keys(&[]) {
        let parts: Vec<&str> = flat.split(crate::SEP).collect();
        let value = host.get(&[key(&flat)]);
        match parts.as_slice() {
            ["c", event, field] => {
                let event = events.entry((*event).to_owned()).or_insert_with(|| Event {
                    id: (*event).to_owned(),
                    title: (*event).to_owned(),
                    start: 0,
                    end: 0,
                    place: String::new(),
                    note: String::new(),
                    yes: Vec::new(),
                    maybe: Vec::new(),
                    no: Vec::new(),
                });
                match *field {
                    "title" => {
                        event.title = value.as_text().unwrap_or(event.id.as_str()).to_owned()
                    }
                    "start" => event.start = as_i64(&value),
                    "end" => event.end = as_i64(&value),
                    "place" => event.place = value.as_text().unwrap_or("").to_owned(),
                    "note" => event.note = value.as_text().unwrap_or("").to_owned(),
                    _ => {}
                }
            }
            ["r", event, person] => {
                if let Some(answer) = value.as_text() {
                    answers
                        .entry((*event).to_owned())
                        .or_default()
                        .insert((*person).to_owned(), answer.to_owned());
                }
            }
            _ => {}
        }
    }
    for (id, event) in &mut events {
        let Some(people) = answers.remove(id) else {
            continue;
        };
        for (person, answer) in people {
            match answer.as_str() {
                "yes" => event.yes.push(person),
                "maybe" => event.maybe.push(person),
                "no" => event.no.push(person),
                _ => {}
            }
        }
        event.yes.sort();
        event.maybe.sort();
        event.no.sort();
    }
    let mut out: Vec<Event> = events.into_values().collect();
    out.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| a.id.cmp(&b.id)));
    out
}

fn resolve_event(host: &Host, wanted: &str) -> Option<String> {
    if wanted.is_empty() {
        return None;
    }
    if !host
        .get(&[key(&crate::join(&["c", wanted, "title"]))])
        .is_null()
    {
        return Some(wanted.to_owned());
    }
    for flat in host.keys(&[]) {
        let parts: Vec<&str> = flat.split(crate::SEP).collect();
        if let ["c", id, "title"] = parts.as_slice() {
            if host.get(&[key(&flat)]).as_text() == Some(wanted) {
                return Some((*id).to_owned());
            }
        }
    }
    None
}

fn delete_prefix(host: &Host, prefix: &str) {
    for flat in host.keys(&[]) {
        if flat.starts_with(prefix) {
            host.delete(&[key(&flat)]);
        }
    }
}

fn as_i64(v: &Cbor) -> i64 {
    v.as_integer()
        .and_then(|i| i64::try_from(i128::from(i)).ok())
        .unwrap_or(0)
}
