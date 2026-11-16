//! Lists: named checklists that everyone in the group ticks off together.
//!
//! Flat state:
//! - `l<US><list><US>name` (the list's id is its name in lower case, so two people who start the
//!   same list at once end up with one)
//! - `i<US><list><US><item><US>text|done`
//!
//! Actions (`payload` is what the button carries, `form.*` the inputs):
//! - `add_list` (`form.list`), `delete_list {list}` (asks twice)
//! - `add_item {list}` (`form.item.<list hash>`), `remove_item {list, item}`
//! - `set_done {list, item}` (from the checkbox, `sync.done.<item hash>`), `clear_done {list}`

use std::collections::BTreeMap;

use ciborium::Value as Cbor;

use crate::act::{form_patch, payload, Act};
use crate::doc::Doc;
use crate::ui::{self, badge, button, card, input, row, text, El, Page};
use crate::value::text as cbor_text;
use crate::Host;

pub const NAME: &str = "Lists";

pub fn form(_host: &Host) -> Cbor {
    crate::value::map([("list", cbor_text("")), ("item", Cbor::Map(Vec::new()))])
}

pub fn action(host: &Host, event: &Cbor) -> Result<(), String> {
    let act = Act::parse(event);
    let doc = Doc::load(host);
    match act.name.as_str() {
        "add_list" => {
            let name = act.form("list");
            if name.is_empty() {
                return Err("Give the list a name first.".into());
            }
            let list = name.to_lowercase();
            if doc.get(&["l", &list, "name"]).is_some() {
                return Err(format!("There is already a list called “{name}”."));
            }
            host.put(
                &[crate::value::key(&crate::join(&["l", &list, "name"]))],
                cbor_text(name),
            );
            ui::signal(host, form_patch(&[("list", cbor_text(""))]));
        }
        "delete_list" => {
            let list = act.arg("list");
            if !ui::confirmed(&format!("delete {list}")) {
                return Ok(());
            }
            delete_all(host, &doc.keys_of(&["l", list]));
            delete_all(host, &doc.keys_of(&["i", list]));
            delete_all(host, &[crate::join(&["l", list, "name"])]);
        }
        "add_item" => {
            let list = act.arg("list");
            let hash = ui::path_id(list);
            let item = act
                .signal(&["form", "item", &hash])
                .and_then(Cbor::as_text)
                .unwrap_or("")
                .trim();
            if item.is_empty() {
                return Err("Write the item first.".into());
            }
            if doc.get(&["l", list, "name"]).is_none() {
                return Err("That list is gone.".into());
            }
            put(host, &["i", list, &act.id, "text"], cbor_text(item));
            put(host, &["i", list, &act.id, "done"], Cbor::Bool(false));
            ui::signal(
                host,
                crate::value::map([(
                    "form",
                    crate::value::map([(
                        "item",
                        Cbor::Map(vec![(cbor_text(hash), cbor_text(""))]),
                    )]),
                )]),
            );
        }
        "remove_item" => {
            delete_all(host, &doc.keys_of(&["i", act.arg("list"), act.arg("item")]));
        }
        "set_done" => {
            let (list, item) = (act.arg("list"), act.arg("item"));
            if doc.get(&["i", list, item, "text"]).is_none() {
                return Ok(());
            }
            let done = act
                .sync_bool(&["done", &ui::path_id(item)])
                .unwrap_or(!doc.bool(&["i", list, item, "done"]));
            put(host, &["i", list, item, "done"], Cbor::Bool(done));
        }
        "clear_done" => {
            let list = act.arg("list");
            for (parts, value) in doc.under(&["i", list]) {
                if let [item, "done"] = parts.as_slice() {
                    if value.as_bool() == Some(true) {
                        delete_all(host, &doc.keys_of(&["i", list, item]));
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

struct List {
    id: String,
    name: String,
    items: Vec<Item>,
}

fn lists(doc: &Doc) -> Vec<List> {
    let mut lists: Vec<List> = doc
        .ids("l")
        .into_iter()
        .filter(|id| doc.get(&["l", id, "name"]).is_some())
        .map(|id| {
            let mut items: BTreeMap<&str, Item> = BTreeMap::new();
            for (parts, value) in doc.under(&["i", &id]) {
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
            List {
                name: doc.str(&["l", &id, "name"]).to_owned(),
                id,
                items,
            }
        })
        .collect();
    lists.sort_by_key(|l| l.name.to_lowercase());
    lists
}

pub fn page(host: &Host) -> Page {
    let doc = Doc::load(host);
    let lists = lists(&doc);
    let mut page = Page::new(NAME);

    page.add(row("new", "root"));
    page.add(
        input("new-name", "new", "New list", "form.list")
            .placeholder("Groceries")
            .on_submit("add_list", Cbor::Null),
    );
    page.add(button(
        "new-add",
        "new",
        "primary",
        "Create list",
        "add_list",
        Cbor::Null,
    ));

    if lists.is_empty() {
        page.add(
            El::new("none", "notice")
                .parent("root")
                .text("No lists yet. Name one above to start; everyone in the group sees it."),
        );
    }

    for list in &lists {
        let hash = ui::path_id(&list.id);
        let card_id = format!("l-{hash}");
        let done = list.items.iter().filter(|i| i.done).count();
        page.add(card(&card_id, "root", &list.name));
        if !list.items.is_empty() {
            page.add(
                El::new(&format!("{card_id}-progress"), "progress")
                    .parent(&card_id)
                    .label(&format!("{done} of {} done", list.items.len()))
                    .set(
                        "value",
                        crate::value::float(done as f64 / list.items.len() as f64),
                    ),
            );
        }
        for item in &list.items {
            let item_hash = ui::path_id(&item.id);
            let row_id = format!("i-{item_hash}");
            let args = payload(&[("list", &list.id), ("item", &item.id)]);
            page.add(row(&row_id, &card_id));
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
        if list.items.is_empty() {
            page.add(text(
                &format!("{card_id}-empty"),
                &card_id,
                "caption",
                "Nothing here yet.",
            ));
        }
        let add = format!("{card_id}-add");
        let args = payload(&[("list", &list.id)]);
        page.add(row(&add, &card_id));
        page.add(
            input(
                &format!("{add}-text"),
                &add,
                "Add an item",
                &format!("form.item.{hash}"),
            )
            .on_submit("add_item", args.clone()),
        );
        page.add(button(
            &format!("{add}-go"),
            &add,
            "secondary",
            "Add",
            "add_item",
            args.clone(),
        ));

        let foot = format!("{card_id}-foot");
        page.add(row(&foot, &card_id).set("justify", cbor_text("between")));
        page.add(
            button(
                &format!("{foot}-clear"),
                &foot,
                "plain",
                "Clear done",
                "clear_done",
                args.clone(),
            )
            .disabled(done == 0),
        );
        let asking = ui::asking(&format!("delete {}", list.id));
        page.add(button(
            &format!("{foot}-del"),
            &foot,
            if asking { "failure" } else { "plain" },
            if asking {
                "Press again to delete"
            } else {
                "Delete list"
            },
            "delete_list",
            args,
        ));
        if done > 0 && done == list.items.len() {
            page.add(badge(
                &format!("{card_id}-all"),
                &card_id,
                "success",
                "All done",
            ));
        }
    }
    page
}
