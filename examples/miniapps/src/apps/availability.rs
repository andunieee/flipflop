//! Availability: a few time slots, and who can make each one.
//!
//! Flat state:
//! - `a<US><plan><US>title|note|by|ts`
//! - `a<US><plan><US>s<US><slot><US>start|end` (epoch ms)
//! - `r<US><plan><US><slot><US><peer>` = `"yes"`, `"maybe"` or `"no"`
//!
//! Actions:
//! - `{name:"create", payload:{title, note?, slots:[{id,start,end}]}}` (or `form.slots` as
//!   `"fri 1000 2000, sat 3000 4000"`)
//! - `{name:"respond", payload:{plan, slot, value:"yes"|"maybe"|"no"}}`
//! - `{name:"remove_plan", payload:{plan}}`

use std::collections::BTreeMap;

use ciborium::Value as Cbor;

use crate::ui::{self, El};
use crate::value::{field, i64_of, key, str_of, text};
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

    match name {
        "create" => {
            let title = str_of(&payload, "title")
                .or_else(|| crate::form_str(&signals, "title"))
                .unwrap_or("")
                .trim()
                .to_owned();
            let slots = slots(&payload, &signals);
            if title.is_empty() || slots.is_empty() {
                ui::log(host, "A plan needs a title and a slot", "warning");
                return;
            }
            for (field, value) in [
                ("title", text(&title)),
                (
                    "note",
                    text(
                        str_of(&payload, "note")
                            .or_else(|| crate::form_str(&signals, "note"))
                            .unwrap_or(""),
                    ),
                ),
                ("by", text(&crate::event_me(event))),
                ("ts", crate::value::int(crate::event_ts(event))),
            ] {
                host.put(&[key(&crate::join(&["a", &id, field]))], value);
            }
            for (slot, start, end) in slots {
                put_slot(host, &id, &slot, start, end);
            }
            ui::signal(
                host,
                Cbor::Map(vec![(
                    text("form"),
                    Cbor::Map(vec![
                        (text("title"), text("")),
                        (text("note"), text("")),
                        (text("slots"), text("")),
                    ]),
                )]),
            );
        }
        "respond" => {
            let (Some(plan), Some(slot), Some(value)) = (
                resolve_plan(host, str_of(&payload, "plan").unwrap_or("")),
                str_of(&payload, "slot"),
                str_of(&payload, "value"),
            ) else {
                return;
            };
            if !matches!(value, "yes" | "maybe" | "no") {
                return;
            }
            host.put(
                &[key(&crate::join(&[
                    "r",
                    &plan,
                    slot,
                    &crate::event_me(event),
                ]))],
                text(value),
            );
        }
        "remove_plan" => {
            if let Some(plan) = resolve_plan(host, str_of(&payload, "plan").unwrap_or("")) {
                delete_prefix(host, &crate::prefix(&["a", &plan]));
                delete_prefix(host, &crate::prefix(&["r", &plan]));
            }
        }
        _ => return,
    }
    // The document write makes the host call us back with `change`, which renders.
}

fn slots(payload: &Cbor, signals: &Cbor) -> Vec<(String, i64, i64)> {
    if let Some(items) = field(payload, "slots").and_then(Cbor::as_array) {
        let slots: Vec<(String, i64, i64)> = items
            .iter()
            .filter_map(|item| {
                Some((
                    str_of(item, "id")?.to_owned(),
                    i64_of(item, "start")?,
                    i64_of(item, "end")?,
                ))
            })
            .collect();
        if !slots.is_empty() {
            return slots;
        }
    }
    str_of(payload, "slots")
        .or_else(|| crate::form_str(signals, "slots"))
        .unwrap_or("")
        .split(',')
        .filter_map(|piece| {
            let mut words = piece.split_whitespace();
            Some((
                words.next()?.to_owned(),
                words.next()?.parse().ok()?,
                words.next()?.parse().ok()?,
            ))
        })
        .collect()
}

fn put_slot(host: &Host, plan: &str, slot: &str, start: i64, end: i64) {
    host.put(
        &[key(&crate::join(&["a", plan, "s", slot, "start"]))],
        crate::value::int(start),
    );
    host.put(
        &[key(&crate::join(&["a", plan, "s", slot, "end"]))],
        crate::value::int(end),
    );
}

fn render(host: &Host, initial: bool) {
    let plans = state(host);
    let mut el = Vec::new();
    el.push(El::new("av", "column").set("gap", text("medium")));
    el.push(
        El::new("av-title", "text")
            .parent("av")
            .variant("h1")
            .text("Availability"),
    );
    el.push(El::new("av-form", "card").parent("av").title("New plan"));
    el.push(
        El::new("av-name", "input")
            .parent("av-form")
            .set("label", text("Title"))
            .bind("form.title"),
    );
    el.push(
        El::new("av-note", "input")
            .parent("av-form")
            .set("label", text("Note"))
            .bind("form.note"),
    );
    el.push(
        El::new("av-slots", "input")
            .parent("av-form")
            .set("label", text("Slots (name start end, …)"))
            .placeholder("fri 1000 2000, sat 3000 4000")
            .bind("form.slots"),
    );
    el.push(
        El::new("av-create", "button")
            .parent("av-form")
            .variant("primary")
            .label("Create plan")
            .on_press("create", Cbor::Null),
    );

    for plan in plans {
        let card = format!("a-{}", ui::path_id(&plan.id));
        el.push(El::new(&card, "card").parent("av").title(&plan.title));
        if !plan.note.is_empty() {
            el.push(
                El::new(&format!("{card}-note"), "text")
                    .parent(&card)
                    .variant("caption")
                    .text(&plan.note),
            );
        }
        el.push(
            El::new(&format!("{card}-del"), "button")
                .parent(&card)
                .variant("plain")
                .label("Remove plan")
                .on_press(
                    "remove_plan",
                    Cbor::Map(vec![(text("plan"), text(&plan.id))]),
                ),
        );
        el.push(
            El::new(&format!("{card}-slots"), "list")
                .parent(&card)
                .empty("No slots."),
        );
        for slot in &plan.slots {
            let row = format!("s-{}", ui::path_id(&format!("{}:{}", plan.id, slot.id)));
            el.push(El::new(&row, "row").parent(&format!("{card}-slots")));
            el.push(
                El::new(&format!("{row}-when"), "text")
                    .parent(&row)
                    .text(&format!(
                        "{} · {}–{}",
                        slot.id,
                        crate::fmt_time(slot.start),
                        crate::fmt_time(slot.end)
                    )),
            );
            if plan.best.as_deref() == Some(slot.id.as_str()) {
                el.push(
                    El::new(&format!("{row}-best"), "badge")
                        .parent(&row)
                        .variant("success")
                        .text("best"),
                );
            }
            el.push(
                El::new(&format!("{row}-score"), "badge")
                    .parent(&row)
                    .text(&format!("{}", slot.yes.len() * 2 + slot.maybe.len())),
            );
            for (value, label, variant) in [
                ("yes", "Yes", "success"),
                ("maybe", "Maybe", "warning"),
                ("no", "No", "failure"),
            ] {
                el.push(
                    El::new(&format!("{row}-{value}"), "button")
                        .parent(&row)
                        .variant(variant)
                        .label(label)
                        .on_press(
                            "respond",
                            Cbor::Map(vec![
                                (text("plan"), text(&plan.id)),
                                (text("slot"), text(&slot.id)),
                                (text("value"), text(value)),
                            ]),
                        ),
                );
            }
            el.push(
                El::new(&format!("{row}-who"), "text")
                    .parent(&row)
                    .variant("caption")
                    .text(&format!(
                        "yes: {} · maybe: {} · no: {}",
                        join(&slot.yes),
                        join(&slot.maybe),
                        join(&slot.no)
                    )),
            );
        }
    }

    if initial {
        ui::start(
            host,
            "Availability",
            Cbor::Map(vec![(
                text("form"),
                Cbor::Map(vec![
                    (text("title"), text("")),
                    (text("note"), text("")),
                    (text("slots"), text("")),
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

struct Plan {
    id: String,
    title: String,
    note: String,
    best: Option<String>,
    slots: Vec<Slot>,
}

struct Slot {
    id: String,
    start: i64,
    end: i64,
    yes: Vec<String>,
    maybe: Vec<String>,
    no: Vec<String>,
}

fn state(host: &Host) -> Vec<Plan> {
    let mut plans: BTreeMap<String, Plan> = BTreeMap::new();
    let mut answers: BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>> = BTreeMap::new();
    for flat in host.keys(&[]) {
        let parts: Vec<&str> = flat.split(crate::SEP).collect();
        let value = host.get(&[key(&flat)]);
        match parts.as_slice() {
            ["a", plan, field] => {
                let plan = plans.entry((*plan).to_owned()).or_insert_with(|| Plan {
                    id: (*plan).to_owned(),
                    title: (*plan).to_owned(),
                    note: String::new(),
                    best: None,
                    slots: Vec::new(),
                });
                match *field {
                    "title" => plan.title = value.as_text().unwrap_or(plan.id.as_str()).to_owned(),
                    "note" => plan.note = value.as_text().unwrap_or("").to_owned(),
                    _ => {}
                }
            }
            ["a", plan, "s", slot, field] => {
                let slots = &mut plans
                    .entry((*plan).to_owned())
                    .or_insert_with(|| Plan {
                        id: (*plan).to_owned(),
                        title: (*plan).to_owned(),
                        note: String::new(),
                        best: None,
                        slots: Vec::new(),
                    })
                    .slots;
                let at = slots.iter().position(|s| s.id == *slot);
                let slot = match at {
                    Some(at) => &mut slots[at],
                    None => {
                        slots.push(Slot {
                            id: (*slot).to_owned(),
                            start: 0,
                            end: 0,
                            yes: Vec::new(),
                            maybe: Vec::new(),
                            no: Vec::new(),
                        });
                        slots.last_mut().unwrap()
                    }
                };
                match *field {
                    "start" => slot.start = as_i64(&value),
                    "end" => slot.end = as_i64(&value),
                    _ => {}
                }
            }
            ["r", plan, slot, person] => {
                if let Some(answer) = value.as_text() {
                    answers
                        .entry((*plan).to_owned())
                        .or_default()
                        .entry((*slot).to_owned())
                        .or_default()
                        .insert((*person).to_owned(), answer.to_owned());
                }
            }
            _ => {}
        }
    }

    for (id, plan) in &mut plans {
        let Some(plan_answers) = answers.remove(id) else {
            continue;
        };
        for slot in &mut plan.slots {
            let Some(who) = plan_answers.get(&slot.id) else {
                continue;
            };
            for (person, answer) in who {
                match answer.as_str() {
                    "yes" => slot.yes.push(person.clone()),
                    "maybe" => slot.maybe.push(person.clone()),
                    "no" => slot.no.push(person.clone()),
                    _ => {}
                }
            }
            slot.yes.sort();
            slot.maybe.sort();
            slot.no.sort();
        }
        plan.best = plan
            .slots
            .iter()
            .max_by(|a, b| {
                (a.yes.len() * 2 + a.maybe.len())
                    .cmp(&(b.yes.len() * 2 + b.maybe.len()))
                    .then_with(|| b.start.cmp(&a.start))
            })
            .map(|s| s.id.clone());
    }

    let mut out: Vec<Plan> = plans.into_values().collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn resolve_plan(host: &Host, wanted: &str) -> Option<String> {
    if wanted.is_empty() {
        return None;
    }
    if !host
        .get(&[key(&crate::join(&["a", wanted, "title"]))])
        .is_null()
    {
        return Some(wanted.to_owned());
    }
    for flat in host.keys(&[]) {
        let parts: Vec<&str> = flat.split(crate::SEP).collect();
        if let ["a", id, "title"] = parts.as_slice() {
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
