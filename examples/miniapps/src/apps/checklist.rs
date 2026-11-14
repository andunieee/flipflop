//! Lists and checklists: named lists of items, each with a checkbox-like done state.
//!
//! Flat state (see the crate docs):
//! - `l<US><list><US>name`, `l<US><list><US>ts`
//! - `i<US><list><US><item><US>text|done|by|ts`
//!
//! Actions:
//! - `{name:"add_list", signals:{form:{list:"..."}}}` (or `payload.name`)
//! - `{name:"remove_list", payload:{list}}`
//! - `{name:"add_item", payload:{list}, signals:{form:{item:{<list hash>:"..."}}}}` (or `payload.text`)
//! - `{name:"toggle_item", payload:{list, item}}`
//! - `{name:"remove_item", payload:{list, item}}`
//! - `{name:"clear_done", payload:{list}}`

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
    let payload = field(event, "payload").cloned().unwrap_or(Cbor::Null);
    let signals = field(event, "signals").cloned().unwrap_or(Cbor::Null);
    let id = new_id(event);

    match name {
        "add_list" => {
            let name = str_of(&payload, "name")
                .or_else(|| str_of(field(&signals, "form").unwrap_or(&Cbor::Null), "list"))
                .unwrap_or("")
                .trim()
                .to_owned();
            if name.is_empty() {
                ui::log(host, "A list needs a name", "warning");
                return;
            }
            host.put(&[key(&crate::join(&["l", &id, "name"]))], text(&name));
            host.put(&[key(&crate::join(&["l", &id, "ts"]))], now(event));
            ui::signal(
                host,
                Cbor::Map(vec![(
                    text("form"),
                    Cbor::Map(vec![(text("list"), text(""))]),
                )]),
            );
        }
        "remove_list" => {
            let Some(list) = resolve_list(host, str_of(&payload, "list").unwrap_or("")) else {
                return;
            };
            delete_prefix(host, &crate::prefix(&["l", &list]));
            delete_prefix(host, &crate::prefix(&["i", &list]));
        }
        "add_item" => {
            let Some(list) = resolve_list(host, str_of(&payload, "list").unwrap_or("")) else {
                return;
            };
            let hash = ui::path_id(&list);
            let item_signals = field(&signals, "form")
                .and_then(|form| field(form, "item"))
                .cloned()
                .unwrap_or(Cbor::Null);
            let body = str_of(&payload, "text")
                .or_else(|| str_of(&item_signals, &hash))
                .unwrap_or("")
                .trim()
                .to_owned();
            if body.is_empty() {
                ui::log(host, "An item needs text", "warning");
                return;
            }
            for (field, value) in [
                ("text", text(&body)),
                ("done", Cbor::Bool(false)),
                ("by", text(str_of(event, "me").unwrap_or(""))),
                ("ts", now(event)),
            ] {
                host.put(&[key(&crate::join(&["i", &list, &id, field]))], value);
            }
            ui::signal(
                host,
                Cbor::Map(vec![(
                    text("form"),
                    Cbor::Map(vec![(
                        text("item"),
                        Cbor::Map(vec![(text(&hash), text(""))]),
                    )]),
                )]),
            );
        }
        "toggle_item" => {
            let (Some(list), Some(item)) = (
                resolve_list(host, str_of(&payload, "list").unwrap_or("")),
                str_of(&payload, "item"),
            ) else {
                return;
            };
            let item = resolve_item(host, &list, item).unwrap_or_else(|| item.to_owned());
            let done = host
                .get(&[key(&crate::join(&["i", &list, &item, "done"]))])
                .as_bool()
                .unwrap_or(false);
            host.put(
                &[key(&crate::join(&["i", &list, &item, "done"]))],
                Cbor::Bool(!done),
            );
        }
        "remove_item" => {
            let (Some(list), Some(item)) = (
                resolve_list(host, str_of(&payload, "list").unwrap_or("")),
                str_of(&payload, "item"),
            ) else {
                return;
            };
            if let Some(item) = resolve_item(host, &list, item) {
                delete_prefix(host, &crate::prefix(&["i", &list, &item]));
            }
        }
        "clear_done" => {
            let Some(list) = resolve_list(host, str_of(&payload, "list").unwrap_or("")) else {
                return;
            };
            let mut done = Vec::new();
            for flat in host.keys(&[]) {
                let parts: Vec<&str> = flat.split(crate::SEP).collect();
                if let ["i", l, item, "done"] = parts.as_slice() {
                    if *l == list && host.get(&[key(&flat)]).as_bool().unwrap_or(false) {
                        done.push((*item).to_owned());
                    }
                }
            }
            for item in done {
                delete_prefix(host, &crate::prefix(&["i", &list, &item]));
            }
        }
        _ => return,
    }
    // The document write makes the host call us back with `change`, which renders.
}

fn render(host: &Host, initial: bool) {
    let lists = state(host);
    let mut el = Vec::new();
    el.push(El::new("cl", "column").set("gap", text("medium")));
    el.push(
        El::new("cl-title", "text")
            .parent("cl")
            .variant("h1")
            .text("Lists"),
    );
    el.push(El::new("cl-new", "row").parent("cl"));
    el.push(
        El::new("cl-new-input", "input")
            .parent("cl-new")
            .set("label", text("New list"))
            .placeholder("Groceries")
            .bind("form.list")
            .on_submit("add_list", Cbor::Null),
    );
    el.push(
        El::new("cl-new-add", "button")
            .parent("cl-new")
            .variant("primary")
            .label("Add list")
            .on_press("add_list", Cbor::Null),
    );

    for list in lists {
        let card = format!("l-{}", ui::path_id(&list.id));
        el.push(El::new(&card, "card").parent("cl").title(&list.name));
        el.push(El::new(&format!("{card}-head"), "row").parent(&card));
        el.push(
            El::new(&format!("{card}-clear"), "button")
                .parent(&format!("{card}-head"))
                .variant("plain")
                .label("Clear done")
                .on_press("clear_done", list_payload(&list.id)),
        );
        el.push(
            El::new(&format!("{card}-del"), "button")
                .parent(&format!("{card}-head"))
                .variant("failure")
                .label("Remove list")
                .on_press("remove_list", list_payload(&list.id)),
        );
        el.push(
            El::new(&format!("{card}-items"), "list")
                .parent(&card)
                .empty("Nothing yet."),
        );
        for item in &list.items {
            let row = format!("i-{}", ui::path_id(&item.id));
            el.push(El::new(&row, "row").parent(&format!("{card}-items")));
            if item.done {
                el.push(
                    El::new(&format!("{row}-done"), "badge")
                        .parent(&row)
                        .variant("success")
                        .text("done"),
                );
            }
            el.push(
                El::new(&format!("{row}-text"), "text")
                    .parent(&row)
                    .text(&item.text),
            );
            el.push(
                El::new(&format!("{row}-toggle"), "button")
                    .parent(&row)
                    .variant("plain")
                    .label(if item.done { "Undo" } else { "Done" })
                    .on_press("toggle_item", item_payload(&list.id, &item.id)),
            );
            el.push(
                El::new(&format!("{row}-del"), "button")
                    .parent(&row)
                    .variant("plain")
                    .label("Remove")
                    .on_press("remove_item", item_payload(&list.id, &item.id)),
            );
        }
        el.push(El::new(&format!("{card}-add"), "row").parent(&card));
        el.push(
            El::new(&format!("{card}-input"), "input")
                .parent(&format!("{card}-add"))
                .set("label", text("Add item"))
                .bind(&format!("form.item.{}", ui::path_id(&list.id)))
                .on_submit("add_item", list_payload(&list.id)),
        );
        el.push(
            El::new(&format!("{card}-addbtn"), "button")
                .parent(&format!("{card}-add"))
                .variant("secondary")
                .label("Add")
                .on_press("add_item", list_payload(&list.id)),
        );
    }

    if initial {
        ui::start(
            host,
            "Checklist",
            Cbor::Map(vec![(
                text("form"),
                Cbor::Map(vec![
                    (text("list"), text("")),
                    (text("item"), Cbor::Map(Vec::new())),
                ]),
            )]),
            el,
        );
    } else {
        ui::update(host, el);
    }
}

fn list_payload(list: &str) -> Cbor {
    Cbor::Map(vec![(text("list"), text(list))])
}

fn item_payload(list: &str, item: &str) -> Cbor {
    Cbor::Map(vec![(text("list"), text(list)), (text("item"), text(item))])
}

fn now(event: &Cbor) -> Cbor {
    crate::value::int(i64_of(event, "ts").unwrap_or(0))
}

fn new_id(event: &Cbor) -> String {
    format!(
        "{}-{}-{}",
        str_of(event, "me").unwrap_or("me"),
        i64_of(event, "ts").unwrap_or(0),
        i64_of(event, "seq").unwrap_or(0)
    )
}

fn resolve_list(host: &Host, wanted: &str) -> Option<String> {
    if wanted.is_empty() {
        return None;
    }
    if !host
        .get(&[key(&crate::join(&["l", wanted, "name"]))])
        .is_null()
    {
        return Some(wanted.to_owned());
    }
    for flat in host.keys(&[]) {
        let parts: Vec<&str> = flat.split(crate::SEP).collect();
        if let ["l", id, "name"] = parts.as_slice() {
            if host.get(&[key(&flat)]).as_text() == Some(wanted) {
                return Some((*id).to_owned());
            }
        }
    }
    None
}

fn resolve_item(host: &Host, list: &str, wanted: &str) -> Option<String> {
    if !host
        .get(&[key(&crate::join(&["i", list, wanted, "text"]))])
        .is_null()
    {
        return Some(wanted.to_owned());
    }
    for flat in host.keys(&[]) {
        let parts: Vec<&str> = flat.split(crate::SEP).collect();
        if let ["i", l, id, "text"] = parts.as_slice() {
            if *l == list && host.get(&[key(&flat)]).as_text() == Some(wanted) {
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

struct ListState {
    id: String,
    name: String,
    ts: i64,
    items: Vec<ItemState>,
}

struct ItemState {
    id: String,
    text: String,
    done: bool,
    ts: i64,
}

fn state(host: &Host) -> Vec<ListState> {
    let mut lists: BTreeMap<String, ListState> = BTreeMap::new();
    let mut items: BTreeMap<String, BTreeMap<String, ItemState>> = BTreeMap::new();
    for flat in host.keys(&[]) {
        let parts: Vec<&str> = flat.split(crate::SEP).collect();
        let value = host.get(&[key(&flat)]);
        match parts.as_slice() {
            ["l", id, "name"] => {
                let list = lists.entry((*id).to_owned()).or_insert_with(|| ListState {
                    id: (*id).to_owned(),
                    name: (*id).to_owned(),
                    ts: 0,
                    items: Vec::new(),
                });
                list.name = value.as_text().unwrap_or(id).to_owned();
            }
            ["l", id, "ts"] => {
                if let Some(list) = lists.get_mut(*id) {
                    list.ts = as_i64(&value);
                }
            }
            ["i", list, item, field] => {
                let item = items
                    .entry((*list).to_owned())
                    .or_default()
                    .entry((*item).to_owned())
                    .or_insert_with(|| ItemState {
                        id: parts[2].to_owned(),
                        text: String::new(),
                        done: false,
                        ts: 0,
                    });
                match *field {
                    "text" => item.text = value.as_text().unwrap_or("").to_owned(),
                    "done" => item.done = value.as_bool().unwrap_or(false),
                    "ts" => item.ts = as_i64(&value),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    for (list, items) in items {
        let list = lists.entry(list.clone()).or_insert_with(|| ListState {
            id: list.clone(),
            name: list,
            ts: 0,
            items: Vec::new(),
        });
        let mut items: Vec<ItemState> = items.into_values().collect();
        items.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.id.cmp(&b.id)));
        list.items = items;
    }
    let mut lists: Vec<ListState> = lists.into_values().collect();
    lists.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.id.cmp(&b.id)));
    lists
}

fn as_i64(v: &Cbor) -> i64 {
    v.as_integer()
        .and_then(|i| i64::try_from(i128::from(i)).ok())
        .unwrap_or(0)
}
