//! A checklist that everyone in the group ticks off together. The list's name is asked when the
//! app is added (the tab is its own name), so one app is one list.
//!
//! Flat state:
//! - `i<US><item><US>text|done`
//!
//! Actions (`payload` is what the button carries, `form.*` the inputs):
//! - `add_item` (`form.item`), `remove_item {item}`
//! - `set_done {item}` (from the checkbox, `sync.done.<item hash>`), `clear_done`

use std::collections::BTreeMap;

use ciborium::Value as Cbor;

use crate::act::{form_patch, payload, Act};
use crate::doc::Doc;
use crate::ui::{self, badge, button, card, input, row, text, El, Page};
use crate::value::text as cbor_text;
use crate::Host;

pub const NAME: &str = "Checklist";

pub fn title() -> String {
    match crate::param("list") {
        name if name.is_empty() => NAME.to_owned(),
        name => name,
    }
}

pub fn params() -> Cbor {
    crate::params(
        NAME,
        NAME,
        vec![],
    )
}

pub fn form(_host: &Host) -> Cbor {
    crate::value::map([("item", cbor_text(""))])
}

pub fn action(host: &Host, event: &Cbor) -> Result<(), String> {
    let act = Act::parse(event);
    let doc = Doc::load(host);
    match act.name.as_str() {
        "add_item" => {
            let item = act.form("item");
            if item.is_empty() {
                return Err("Write the item first.".into());
            }
            put(host, &["i", &act.id, "text"], cbor_text(item));
            put(host, &["i", &act.id, "done"], Cbor::Bool(false));
            ui::signal(host, form_patch(&[("item", cbor_text(""))]));
        }
        "remove_item" => {
            delete_all(host, &doc.keys_of(&["i", act.arg("item")]));
        }
        "set_done" => {
            let item = act.arg("item");
            if doc.get(&["i", item, "text"]).is_none() {
                return Ok(());
            }
            let done = act
                .sync_bool(&["done", &ui::path_id(item)])
                .unwrap_or(!doc.bool(&["i", item, "done"]));
            put(host, &["i", item, "done"], Cbor::Bool(done));
        }
        "clear_done" => {
            for (parts, value) in doc.under(&["i"]) {
                if let [item, "done"] = parts.as_slice() {
                    if value.as_bool() == Some(true) {
                        delete_all(host, &doc.keys_of(&["i", item]));
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn put(host: &Host, parts: &[&str], value: Cbor) {
    host.put(&[crate::value::key(&crate::join(parts))], value);
}

fn delete_all(host: &Host, keys: &[String]) {
    for key in keys {
        host.delete(&[crate::value::key(key)]);
    }
}

struct Item {
    id: String,
    text: String,
    done: bool,
}

fn items(doc: &Doc) -> Vec<Item> {
    let mut items: BTreeMap<&str, Item> = BTreeMap::new();
    for (parts, value) in doc.under(&["i"]) {
        let [item, field] = parts.as_slice() else {
            continue;
        };
        let entry = items.entry(item).or_insert_with(|| Item {
            id: (*item).to_owned(),
            text: String::new(),
            done: false,
        });
        match *field {
            "text" => entry.text = value.as_text().unwrap_or("").to_owned(),
            "done" => entry.done = value.as_bool().unwrap_or(false),
            _ => {}
        }
    }
    // The item ids start with the peer and the time, so this is roughly the order added;
    // sort by time to make it so.
    let mut items: Vec<Item> = items.into_values().filter(|i| !i.text.is_empty()).collect();
    items.sort_by_key(|i| {
        i.id.rsplit('-')
            .nth(1)
            .and_then(|t| t.parse::<i64>().ok())
            .unwrap_or(0)
    });
    items
}

pub fn page(host: &Host) -> Page {
    let doc = Doc::load(host);
    let items = items(&doc);
    let title = title();
    let mut page = Page::new(&title);

    let done = items.iter().filter(|i| i.done).count();
    page.add(card("list", "root", &title));
    if !items.is_empty() {
        page.add(
            El::new("list-progress", "progress")
                .parent("list")
                .label(&format!("{done} of {} done", items.len()))
                .set(
                    "value",
                    crate::value::float(done as f64 / items.len() as f64),
                ),
        );
    }
    for item in &items {
        let item_hash = ui::path_id(&item.id);
        let row_id = format!("i-{item_hash}");
        let args = payload(&[("item", &item.id)]);
        page.add(row(&row_id, "list"));
        page.add(
            El::new(&format!("{row_id}-done"), "checkbox")
                .parent(&row_id)
                .label(&item.text)
                .bind(&format!("sync.done.{item_hash}"))
                .on_change("set_done", args.clone()),
        );
        page.sync(&format!("done.{item_hash}"), Cbor::Bool(item.done));
        page.add(button(
            &format!("{row_id}-del"),
            &row_id,
            "plain",
            "Remove",
            "remove_item",
            args,
        ));
    }
    if items.is_empty() {
        page.add(text("list-empty", "list", "caption", "Nothing here yet."));
    }
    page.add(row("add", "list"));
    page.add(
        input("add-text", "add", "Add an item", "form.item").on_submit("add_item", Cbor::Null),
    );
    page.add(button(
        "add-go",
        "add",
        "secondary",
        "Add",
        "add_item",
        Cbor::Null,
    ));

    page.add(row("foot", "list").set("justify", cbor_text("between")));
    page.add(
        button(
            "foot-clear",
            "foot",
            "plain",
            "Clear done",
            "clear_done",
            Cbor::Null,
        )
        .disabled(done == 0),
    );
    if done > 0 && done == items.len() {
        page.add(badge("list-all", "list", "success", "All done"));
    }
    page
}
