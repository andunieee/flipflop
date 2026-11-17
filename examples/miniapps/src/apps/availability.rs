//! Finding a time: a few possible times, and who can make each one.
//!
//! A time is picked with selects (day, month, year, hour, minute) in the user's own zone and stored
//! as UTC milliseconds.
//!
//! Flat state:
//! - `a<US><plan><US>title|note|by`
//! - `s<US><plan><US><slot><US>start` (epoch ms)
//! - `v<US><plan><US><slot><US><peer>` = `"yes"`, `"maybe"` or `"no"`
//!
//! Actions:
//! - `create` (`form.title`, `form.note`), `add_slot {plan}` (the picker at `form.slot.<plan hash>`)
//! - `respond {plan, slot}` (the answer is in `sync.v.<plan hash>.<slot hash>`)
//! - `remove_plan {plan}` (asks twice)

use ciborium::Value as Cbor;

use crate::act::{payload, Act};
use crate::doc::{Doc, People};
use crate::picker;
use crate::time;
use crate::ui::{self, badge, button, card, input, row, text, El, Page};
use crate::value::{int, key, map, text as cbor_text};
use crate::Host;

pub const NAME: &str = "Find a time";

pub fn title() -> String {
    NAME.to_owned()
}

/// Nothing to ask: the app starts empty.
pub fn params() -> Cbor {
    crate::params(Vec::new())
}

pub fn form(host: &Host) -> Cbor {
    let doc = Doc::load(host);
    let slot = doc
        .ids("a")
        .iter()
        .map(|plan| {
            let hash = ui::path_id(plan);
            picker::mark(&["slot", &hash]);
            (cbor_text(hash), picker::initial())
        })
        .collect();
    map([
        ("title", cbor_text("")),
        ("note", cbor_text("")),
        ("slot", Cbor::Map(slot)),
    ])
}

pub fn action(host: &Host, event: &Cbor) -> Result<(), String> {
    let act = Act::parse(event);
    let doc = Doc::load(host);
    match act.name.as_str() {
        "create" => {
            let title = act.form("title");
            if title.is_empty() {
                return Err("What is it for? Give the plan a title.".into());
            }
            put(host, &["a", &act.id, "title"], cbor_text(title));
            put(host, &["a", &act.id, "note"], cbor_text(act.form("note")));
            put(host, &["a", &act.id, "by"], cbor_text(&act.me));
            let hash = ui::path_id(&act.id);
            picker::mark(&["slot", &hash]);
            ui::signal(
                host,
                map([(
                    "form",
                    map([
                        ("title", cbor_text("")),
                        ("note", cbor_text("")),
                        (
                            "slot",
                            Cbor::Map(vec![(cbor_text(hash), picker::initial())]),
                        ),
                    ]),
                )]),
            );
        }
        "add_slot" => {
            let plan = act.arg("plan");
            let start = picker::read(&act, &["slot", &ui::path_id(plan)], true)?;
            if doc.get(&["a", plan, "title"]).is_none() {
                return Err("That plan is gone.".into());
            }
            let taken = doc.under(&["s", plan]).iter().any(|(p, v)| {
                matches!(p.as_slice(), [_, "start"])
                    && v.as_integer()
                        .and_then(|i| i64::try_from(i128::from(i)).ok())
                        == Some(start)
            });
            if taken {
                return Err("That time is already on the plan.".into());
            }
            put(host, &["s", plan, &act.id, "start"], int(start));
        }
        "respond" => {
            let (plan, slot) = (act.arg("plan"), act.arg("slot"));
            let answer = act.sync_str(&["v", &ui::path_id(plan), &ui::path_id(slot)]);
            if doc.get(&["s", plan, slot, "start"]).is_some()
                && matches!(answer, "yes" | "maybe" | "no")
            {
                put(host, &["v", plan, slot, &act.me], cbor_text(answer));
            }
        }
        "remove_plan" => {
            let plan = act.arg("plan");
            if !ui::confirmed(&format!("remove {plan}")) {
                return Ok(());
            }
            for flat in ["a", "s", "v"]
                .iter()
                .flat_map(|tag| doc.keys_of(&[tag, plan]))
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

struct Slot {
    id: String,
    start: i64,
    yes: Vec<String>,
    maybe: Vec<String>,
    no: Vec<String>,
    mine: String,
}

impl Slot {
    /// Yes counts twice as much as maybe.
    fn score(&self) -> usize {
        self.yes.len() * 2 + self.maybe.len()
    }
}

fn slots(doc: &Doc, plan: &str, me: &str) -> Vec<Slot> {
    let mut slots: Vec<Slot> = doc
        .under(&["s", plan])
        .into_iter()
        .filter_map(|(parts, value)| match parts.as_slice() {
            [slot, "start"] => Some((
                *slot,
                value
                    .as_integer()
                    .and_then(|i| i64::try_from(i128::from(i)).ok())?,
            )),
            _ => None,
        })
        .map(|(slot, start)| {
            let mut s = Slot {
                id: slot.to_owned(),
                start,
                yes: Vec::new(),
                maybe: Vec::new(),
                no: Vec::new(),
                mine: doc.str(&["v", plan, slot, me]).to_owned(),
            };
            for (parts, value) in doc.under(&["v", plan, slot]) {
                let [peer] = parts.as_slice() else { continue };
                match value.as_text() {
                    Some("yes") => s.yes.push((*peer).to_owned()),
                    Some("maybe") => s.maybe.push((*peer).to_owned()),
                    Some("no") => s.no.push((*peer).to_owned()),
                    _ => {}
                }
            }
            s
        })
        .collect();
    slots.sort_by(|a, b| (a.start, &a.id).cmp(&(b.start, &b.id)));
    slots
}

pub fn page(host: &Host) -> Page {
    let doc = Doc::load(host);
    let people = People::load(host);
    let me = crate::me();
    let mut page = Page::new(NAME);

    page.add(card("new", "root", ""));
    page.add(
        input("new-title", "new", "What is it for?", "form.title").placeholder("Board game night"),
    );
    page.add(input("new-note", "new", "Note (optional)", "form.note"));
    page.add(button(
        "new-go",
        "new",
        "primary",
        "+",
        "create",
        Cbor::Null,
    ));

    let mut plans = doc.ids("a");
    plans.retain(|id| doc.get(&["a", id, "title"]).is_some());
    plans.sort_by_key(|id| {
        std::cmp::Reverse(
            id.rsplit('-')
                .nth(1)
                .and_then(|t| t.parse::<i64>().ok())
                .unwrap_or(0),
        )
    });

    for plan in &plans {
        let hash = ui::path_id(plan);
        let id = format!("a-{hash}");
        let slots = slots(&doc, plan, &me);
        let best = slots.iter().map(Slot::score).max().unwrap_or(0);
        page.add(card(&id, "root", doc.str(&["a", plan, "title"])));
        let note = doc.str(&["a", plan, "note"]);
        if !note.is_empty() {
            page.add(text(&format!("{id}-note"), &id, "caption", note));
        }
        for slot in &slots {
            let slot_hash = ui::path_id(&slot.id);
            let line = format!("{id}-s{slot_hash}");
            let args = payload(&[("plan", plan), ("slot", &slot.id)]);
            page.add(row(&line, &id));
            page.add(text(
                &format!("{line}-when"),
                &line,
                "body",
                &time::when(slot.start),
            ));
            if best > 0 && slot.score() == best {
                page.add(badge(&format!("{line}-best"), &line, "success", "Best"));
            }
            page.add(
                El::new(&format!("{line}-ans"), "select")
                    .parent(&id)
                    .variant("segmented")
                    .bind(&format!("sync.v.{hash}.{slot_hash}"))
                    .set(
                        "options",
                        Cbor::Array(
                            [("yes", "Yes"), ("maybe", "Maybe"), ("no", "No")]
                                .iter()
                                .map(|(v, l)| {
                                    map([("value", cbor_text(*v)), ("label", cbor_text(*l))])
                                })
                                .collect(),
                        ),
                    )
                    .on_change("respond", args.clone()),
            );
            page.sync(&format!("v.{hash}.{slot_hash}"), cbor_text(&slot.mine));
            let mut who = Vec::new();
            for (label, ids) in [("Yes", &slot.yes), ("Maybe", &slot.maybe), ("No", &slot.no)] {
                if !ids.is_empty() {
                    who.push(format!("{label}: {}", people.list(ids)));
                }
            }
            if !who.is_empty() {
                page.add(text(
                    &format!("{line}-who"),
                    &id,
                    "caption",
                    &who.join(" · "),
                ));
            }
        }

        picker::ensure(host, &["slot", &hash]);
        let add = format!("{id}-add");
        page.add(text(&format!("{add}-t"), &id, "caption", "Suggest a time"));
        picker::add(&mut page, &add, &id, &["slot", &hash], true);
        page.add(button(
            &format!("{add}-go"),
            &id,
            "secondary",
            "Add this time",
            "add_slot",
            payload(&[("plan", plan)]),
        ));

        let asking = ui::asking(&format!("remove {plan}"));
        page.add(row(&format!("{id}-foot"), &id).set("justify", cbor_text("end")));
        page.add(button(
            &format!("{id}-foot-del"),
            &format!("{id}-foot"),
            if asking { "failure" } else { "plain" },
            if asking {
                "Press again to delete"
            } else {
                "Delete plan"
            },
            "remove_plan",
            payload(&[("plan", plan)]),
        ));
    }
    page
}
