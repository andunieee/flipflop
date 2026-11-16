//! A shared calendar: events with a date, a time and a length, and who is coming.
//!
//! The day and time are picked with selects, in the user's own zone, and stored as UTC milliseconds.
//!
//! Flat state:
//! - `c<US><event><US>title|start|end|all_day|place|note|by`
//! - `r<US><event><US><peer>` = `"yes"`, `"maybe"` or `"no"`
//!
//! Actions:
//! - `add_event` (`form.title|when|length|place|note`), `pick_day {path, day}` (Today and Tomorrow)
//! - `rsvp {event}` (the answer is in `sync.rsvp.<event hash>`)
//! - `remove_event {event}` (asks twice)

use ciborium::Value as Cbor;

use crate::act::{form_patch, payload, Act};
use crate::doc::{Doc, People};
use crate::picker;
use crate::time;
use crate::ui::{self, badge, button, card, column, input, row, text, El, Page};
use crate::value::{int, key, map, text as cbor_text};
use crate::Host;

pub const NAME: &str = "Calendar";

const LENGTHS: [(&str, &str); 6] = [
    ("30", "30 min"),
    ("60", "1 hour"),
    ("90", "1½ hours"),
    ("120", "2 hours"),
    ("180", "3 hours"),
    ("day", "All day"),
];

pub fn form(_host: &Host) -> Cbor {
    map([
        ("title", cbor_text("")),
        ("when", picker::initial()),
        ("length", cbor_text("60")),
        ("place", cbor_text("")),
        ("note", cbor_text("")),
    ])
}

pub fn action(host: &Host, event: &Cbor) -> Result<(), String> {
    let act = Act::parse(event);
    let doc = Doc::load(host);
    match act.name.as_str() {
        "pick_day" => {
            if let Some(patch) = picker::pick_patch(&act) {
                ui::signal(host, patch);
            }
        }
        "add_event" => {
            let title = act.form("title");
            if title.is_empty() {
                return Err("Give the event a title.".into());
            }
            let start = picker::read(&act, &["when"], true)?;
            let length = act.form("length");
            let (start, end, all_day) = if length == "day" {
                let day = time::day_of(start);
                (time::moment(day, 0), time::moment(day + 1, 0), true)
            } else {
                let minutes = length.parse::<i64>().unwrap_or(60);
                (start, start + minutes * 60_000, false)
            };
            for (field, value) in [
                ("title", cbor_text(title)),
                ("start", int(start)),
                ("end", int(end)),
                ("all_day", Cbor::Bool(all_day)),
                ("place", cbor_text(act.form("place"))),
                ("note", cbor_text(act.form("note"))),
                ("by", cbor_text(&act.me)),
            ] {
                put(host, &["c", &act.id, field], value);
            }
            put(host, &["r", &act.id, &act.me], cbor_text("yes"));
            ui::signal(
                host,
                form_patch(&[
                    ("title", cbor_text("")),
                    ("place", cbor_text("")),
                    ("note", cbor_text("")),
                ]),
            );
        }
        "rsvp" => {
            let event = act.arg("event");
            let answer = act.sync_str(&["rsvp", &ui::path_id(event)]);
            if doc.get(&["c", event, "title"]).is_some() && matches!(answer, "yes" | "maybe" | "no")
            {
                put(host, &["r", event, &act.me], cbor_text(answer));
            }
        }
        "remove_event" => {
            let event = act.arg("event");
            if !ui::confirmed(&format!("remove {event}")) {
                return Ok(());
            }
            for flat in doc
                .keys_of(&["c", event])
                .into_iter()
                .chain(doc.keys_of(&["r", event]))
            {
                host.delete(&[key(&flat)]);
            }
        }
        _ => {}
    }
    Ok(())
}

fn put(host: &Host, parts: &[&str], value: Cbor) {
    host.put(&[key(&crate::join(parts))], value);
}

struct Event {
    id: String,
    title: String,
    start: i64,
    end: i64,
    all_day: bool,
    place: String,
    note: String,
    yes: Vec<String>,
    maybe: Vec<String>,
    no: Vec<String>,
    mine: String,
}

fn events(doc: &Doc, me: &str) -> Vec<Event> {
    let mut events: Vec<Event> = doc
        .ids("c")
        .into_iter()
        .filter(|id| doc.get(&["c", id, "title"]).is_some())
        .map(|id| {
            let mut event = Event {
                title: doc.str(&["c", &id, "title"]).to_owned(),
                start: doc.int(&["c", &id, "start"]),
                end: doc.int(&["c", &id, "end"]),
                all_day: doc.bool(&["c", &id, "all_day"]),
                place: doc.str(&["c", &id, "place"]).to_owned(),
                note: doc.str(&["c", &id, "note"]).to_owned(),
                yes: Vec::new(),
                maybe: Vec::new(),
                no: Vec::new(),
                mine: doc.str(&["r", &id, me]).to_owned(),
                id,
            };
            for (parts, value) in doc.under(&["r", &event.id]) {
                let [peer] = parts.as_slice() else { continue };
                match value.as_text() {
                    Some("yes") => event.yes.push((*peer).to_owned()),
                    Some("maybe") => event.maybe.push((*peer).to_owned()),
                    Some("no") => event.no.push((*peer).to_owned()),
                    _ => {}
                }
            }
            event
        })
        .collect();
    events.sort_by(|a, b| (a.start, &a.id).cmp(&(b.start, &b.id)));
    events
}

fn options(pairs: &[(&str, &str)]) -> Cbor {
    Cbor::Array(
        pairs
            .iter()
            .map(|(value, label)| map([("value", cbor_text(*value)), ("label", cbor_text(*label))]))
            .collect(),
    )
}

pub fn page(host: &Host) -> Page {
    let doc = Doc::load(host);
    let people = People::load(host);
    let events = events(&doc, &crate::me());
    let now = time::now();
    let mut page = Page::new(NAME);

    // The form.
    page.add(card("new", "root", "New event"));
    page.add(input("new-title", "new", "What", "form.title").placeholder("Dinner at Ana's"));
    picker::add(&mut page, "new-when", "new", &["when"], true);
    picker::quick_buttons(&mut page, "new-quick", "new", "when");
    page.add(
        El::new("new-length", "select")
            .parent("new")
            .label("Lasts")
            .bind("form.length")
            .set("options", options(&LENGTHS)),
    );
    page.add(row("new-where", "new"));
    page.add(input(
        "new-place",
        "new-where",
        "Where (optional)",
        "form.place",
    ));
    page.add(input(
        "new-note",
        "new-where",
        "Note (optional)",
        "form.note",
    ));
    page.add(button(
        "new-add",
        "new",
        "primary",
        "Add to calendar",
        "add_event",
        Cbor::Null,
    ));

    // Upcoming events, a heading for each day; then what has passed.
    let (past, upcoming): (Vec<&Event>, Vec<&Event>) = events.iter().partition(|e| e.end <= now);
    if upcoming.is_empty() {
        page.add(
            El::new("none", "notice")
                .parent("root")
                .text("Nothing coming up. Add the first event above."),
        );
    }
    let mut last_day = None;
    for event in &upcoming {
        let day = time::day_of(event.start);
        if last_day != Some(day) {
            last_day = Some(day);
            page.add(text(
                &format!("day-{day}"),
                "root",
                "h2",
                &time::day_label(day),
            ));
        }
        event_card(&mut page, event, &people, "root");
    }
    if !past.is_empty() {
        page.add(text("past", "root", "h2", "Earlier"));
        for event in past.iter().rev() {
            event_card(&mut page, event, &people, "root");
        }
    }
    page
}

fn event_card(page: &mut Page, event: &Event, people: &People, parent: &str) {
    let hash = ui::path_id(&event.id);
    let id = format!("ev-{hash}");
    page.add(card(&id, parent, &event.title));

    let when = if event.all_day {
        format!("{}, all day", time::day_label(time::day_of(event.start)))
    } else if time::day_of(event.end - 1) == time::day_of(event.start) {
        format!(
            "{} – {}",
            time::clock(time::minute_of(event.start)),
            time::clock(time::minute_of(event.end))
        )
    } else {
        format!("{} – {}", time::when(event.start), time::when(event.end))
    };
    page.add(row(&format!("{id}-when"), &id));
    page.add(text(
        &format!("{id}-time"),
        &format!("{id}-when"),
        "body",
        &when,
    ));
    if event.start <= time::now() && time::now() < event.end {
        page.add(badge(
            &format!("{id}-now"),
            &format!("{id}-when"),
            "info",
            "Now",
        ));
    }
    if !event.place.is_empty() {
        page.add(text(
            &format!("{id}-place"),
            &id,
            "caption",
            &format!("At {}", event.place),
        ));
    }
    if !event.note.is_empty() {
        page.add(text(&format!("{id}-note"), &id, "caption", &event.note));
    }

    page.add(
        El::new(&format!("{id}-rsvp"), "select")
            .parent(&id)
            .variant("segmented")
            .bind(&format!("sync.rsvp.{hash}"))
            .set(
                "options",
                options(&[("yes", "Going"), ("maybe", "Maybe"), ("no", "Can't")]),
            )
            .on_change("rsvp", payload(&[("event", &event.id)])),
    );
    page.sync(&format!("rsvp.{hash}"), cbor_text(&event.mine));

    let mut who = Vec::new();
    for (label, ids) in [
        ("Going", &event.yes),
        ("Maybe", &event.maybe),
        ("Can't", &event.no),
    ] {
        if !ids.is_empty() {
            who.push(format!("{label}: {}", people.list(ids)));
        }
    }
    if who.is_empty() {
        who.push("No answers yet.".to_owned());
    }
    page.add(column(&format!("{id}-who"), &id).set("gap", cbor_text("none")));
    for (i, line) in who.iter().enumerate() {
        page.add(text(
            &format!("{id}-who{i}"),
            &format!("{id}-who"),
            "caption",
            line,
        ));
    }
    let asking = ui::asking(&format!("remove {}", event.id));
    page.add(button(
        &format!("{id}-del"),
        &id,
        if asking { "failure" } else { "plain" },
        if asking {
            "Press again to remove"
        } else {
            "Remove"
        },
        "remove_event",
        payload(&[("event", &event.id)]),
    ));
}
