//! Polls: a question, a few options, and everyone's answer.
//!
//! Flat state:
//! - `p<US><poll><US>question|multi|closed|by`
//! - `o<US><poll><US><n><US>text` (`n` is the option's number, in the order they were written)
//! - `b<US><poll><US><voter><US><n>` = whether that voter chose that option
//!
//! Actions:
//! - `create` (`form.question`, `form.options` one per line, `form.multi`)
//! - `vote {poll, option}`: in a single-answer poll this replaces the voter's answer; in a
//!   several-answers poll it ticks or unticks the option. A second press on a chosen option takes
//!   the vote back.
//! - `close {poll}`, `reopen {poll}`, `remove_poll {poll}` (asks twice)

use ciborium::Value as Cbor;

use crate::act::{form_patch, payload, Act};
use crate::doc::{Doc, People};
use crate::ui::{self, badge, button, card, input, row, text, El, Page};
use crate::value::{key, map, text as cbor_text};
use crate::Host;

pub const NAME: &str = "Polls";

pub fn title() -> String {
    NAME.to_owned()
}

/// Nothing to ask: the app starts empty.
pub fn params() -> Cbor {
    crate::params(Vec::new())
}

pub fn form(_host: &Host) -> Cbor {
    map([
        ("question", cbor_text("")),
        ("options", cbor_text("")),
        ("multi", Cbor::Bool(false)),
    ])
}

pub fn action(host: &Host, event: &Cbor) -> Result<(), String> {
    let act = Act::parse(event);
    let doc = Doc::load(host);
    match act.name.as_str() {
        "create" => {
            let question = act.form("question");
            if question.is_empty() {
                return Err("Ask a question first.".into());
            }
            let options: Vec<&str> = act
                .signal(&["form", "options"])
                .and_then(Cbor::as_text)
                .unwrap_or("")
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect();
            if options.len() < 2 {
                return Err("Write at least two options, one per line.".into());
            }
            let multi = act.form_bool(&["form", "multi"]).unwrap_or(false);
            put(host, &["p", &act.id, "question"], cbor_text(question));
            put(host, &["p", &act.id, "multi"], Cbor::Bool(multi));
            put(host, &["p", &act.id, "closed"], Cbor::Bool(false));
            put(host, &["p", &act.id, "by"], cbor_text(&act.me));
            for (n, option) in options.iter().enumerate() {
                put(
                    host,
                    &["o", &act.id, &n.to_string(), "text"],
                    cbor_text(*option),
                );
            }
            ui::signal(
                host,
                form_patch(&[
                    ("question", cbor_text("")),
                    ("options", cbor_text("")),
                    ("multi", Cbor::Bool(false)),
                ]),
            );
        }
        "vote" => {
            let (poll, option) = (act.arg("poll"), act.arg("option"));
            if doc.get(&["p", poll, "question"]).is_none() {
                return Ok(());
            }
            if doc.bool(&["p", poll, "closed"]) {
                return Err("That poll is closed.".into());
            }
            let was = doc.bool(&["b", poll, &act.me, option]);
            if !doc.bool(&["p", poll, "multi"]) {
                for (parts, value) in doc.under(&["b", poll, &act.me]) {
                    if value.as_bool() == Some(true) && parts != [option] {
                        put(host, &["b", poll, &act.me, parts[0]], Cbor::Bool(false));
                    }
                }
            }
            put(host, &["b", poll, &act.me, option], Cbor::Bool(!was));
        }
        "close" | "reopen" => {
            let poll = act.arg("poll");
            if doc.get(&["p", poll, "question"]).is_some() {
                put(
                    host,
                    &["p", poll, "closed"],
                    Cbor::Bool(act.name == "close"),
                );
            }
        }
        "remove_poll" => {
            let poll = act.arg("poll");
            if !ui::confirmed(&format!("remove {poll}")) {
                return Ok(());
            }
            for flat in ["p", "o", "b"]
                .iter()
                .flat_map(|tag| doc.keys_of(&[tag, poll]))
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

pub fn page(host: &Host) -> Page {
    let doc = Doc::load(host);
    let people = People::load(host);
    let me = crate::me();
    let mut page = Page::new(NAME);

    page.add(card("new", "root", ""));
    page.add(input("new-q", "new", "Question", "form.question").placeholder("Where shall we eat?"));
    page.add(
        input("new-o", "new", "Options, one per line", "form.options")
            .variant("multiline")
            .placeholder("Pizza\nSushi\nTacos"),
    );
    page.add(
        El::new("new-multi", "checkbox")
            .parent("new")
            .label("Allow choosing several options")
            .bind("form.multi"),
    );
    page.add(button(
        "new-go",
        "new",
        "primary",
        "+",
        "create",
        Cbor::Null,
    ));

    let mut polls = doc.ids("p");
    polls.retain(|id| doc.get(&["p", id, "question"]).is_some());
    // Newest first: ids start with the peer, then the time.
    polls.sort_by_key(|id| {
        std::cmp::Reverse(
            id.rsplit('-')
                .nth(1)
                .and_then(|t| t.parse::<i64>().ok())
                .unwrap_or(0),
        )
    });

    for poll in &polls {
        let hash = ui::path_id(poll);
        let id = format!("p-{hash}");
        let closed = doc.bool(&["p", poll, "closed"]);
        let multi = doc.bool(&["p", poll, "multi"]);
        let options: Vec<(String, String)> = doc
            .under(&["o", poll])
            .into_iter()
            .filter_map(|(parts, value)| match parts.as_slice() {
                [n, "text"] => Some(((*n).to_owned(), value.as_text()?.to_owned())),
                _ => None,
            })
            .collect();
        let mut options = options;
        options.sort_by_key(|(n, _)| n.parse::<usize>().unwrap_or(0));

        // Who chose what.
        let mut chose: Vec<Vec<String>> = vec![Vec::new(); options.len()];
        let mut voters = std::collections::BTreeSet::new();
        for (parts, value) in doc.under(&["b", poll]) {
            let [voter, n] = parts.as_slice() else {
                continue;
            };
            if value.as_bool() != Some(true) {
                continue;
            }
            if let Some(i) = options.iter().position(|(k, _)| k == n) {
                chose[i].push((*voter).to_owned());
                voters.insert(*voter);
            }
        }
        let top = chose.iter().map(Vec::len).max().unwrap_or(0);

        page.add(card(&id, "root", doc.str(&["p", poll, "question"])));
        let meta = format!(
            "{} · {}{}",
            match voters.len() {
                0 => "No votes yet".to_owned(),
                1 => "1 person voted".to_owned(),
                n => format!("{n} people voted"),
            },
            if multi {
                "several answers allowed"
            } else {
                "one answer"
            },
            if closed { " · closed" } else { "" }
        );
        page.add(row(&format!("{id}-meta"), &id));
        page.add(text(
            &format!("{id}-meta-t"),
            &format!("{id}-meta"),
            "caption",
            &meta,
        ));
        if closed {
            page.add(badge(
                &format!("{id}-closed"),
                &format!("{id}-meta"),
                "neutral",
                "Closed",
            ));
        }

        for (i, (n, label)) in options.iter().enumerate() {
            let line = format!("{id}-o{i}");
            let mine = chose[i].contains(&me);
            let share = if voters.is_empty() {
                0.0
            } else {
                chose[i].len() as f64 / voters.len() as f64
            };
            let mut names = people.list(&chose[i]);
            if names.is_empty() {
                names = "no votes".to_owned();
            }
            page.add(row(&line, &id));
            page.add(
                button(
                    &format!("{line}-vote"),
                    &line,
                    if mine { "primary" } else { "secondary" },
                    label,
                    "vote",
                    payload(&[("poll", poll), ("option", n)]),
                )
                .disabled(closed),
            );
            page.add(
                El::new(&format!("{line}-bar"), "progress")
                    .parent(&line)
                    .label(&format!("{} · {names}", chose[i].len()))
                    .set("value", crate::value::float(share)),
            );
            if closed && top > 0 && chose[i].len() == top {
                page.add(badge(&format!("{line}-win"), &line, "success", "Top"));
            }
        }

        let foot = format!("{id}-foot");
        let args = payload(&[("poll", poll)]);
        page.add(row(&foot, &id).set("justify", cbor_text("between")));
        page.add(button(
            &format!("{foot}-close"),
            &foot,
            "plain",
            if closed { "Reopen" } else { "Close poll" },
            if closed { "reopen" } else { "close" },
            args.clone(),
        ));
        let asking = ui::asking(&format!("remove {poll}"));
        page.add(button(
            &format!("{foot}-del"),
            &foot,
            if asking { "failure" } else { "plain" },
            if asking {
                "Press again to delete"
            } else {
                "Delete"
            },
            "remove_poll",
            args,
        ));
    }
    page
}
