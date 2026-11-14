//! Polls: a question, options, and each person's vote.
//!
//! Flat state:
//! - `p<US><poll><US>question|multi|closed|by|ts`
//! - `p<US><poll><US>o<US><option><US>text|ts`
//! - `b<US><poll><US><voter><US><option>` = the timestamp of that vote. The latest vote wins; a
//!   single-choice poll counts each voter's latest option, a multi-choice one counts every option
//!   they marked.
//!
//! Actions:
//! - `{name:"create", payload:{question, options:["a","b"], multi?}}` (`options` may also be the
//!   comma-separated `form.options`)
//! - `{name:"vote", payload:{poll, option}}`
//! - `{name:"close", payload:{poll}}`
//! - `{name:"remove_poll", payload:{poll}}`

use std::collections::BTreeMap;

use ciborium::Value as Cbor;

use crate::ui::{self, El};
use crate::value::{bool_of, field, key, str_of, text};
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
            let question = str_of(&payload, "question")
                .or_else(|| crate::form_str(&signals, "question"))
                .unwrap_or("")
                .trim()
                .to_owned();
            let options = options(&payload, &signals);
            if question.is_empty() || options.len() < 2 {
                ui::log(host, "A poll needs a question and two options", "warning");
                return;
            }
            for (field, value) in [
                ("question", text(&question)),
                (
                    "multi",
                    Cbor::Bool(bool_of(&payload, "multi").unwrap_or(false)),
                ),
                ("closed", Cbor::Bool(false)),
                ("by", text(&crate::event_me(event))),
                ("ts", crate::value::int(crate::event_ts(event))),
            ] {
                host.put(&[key(&crate::join(&["p", &id, field]))], value);
            }
            for (i, option) in options.iter().enumerate() {
                host.put(
                    &[key(&crate::join(&["p", &id, "o", option, "text"]))],
                    text(option),
                );
                host.put(
                    &[key(&crate::join(&["p", &id, "o", option, "ts"]))],
                    crate::value::int(i as i64),
                );
            }
            ui::signal(
                host,
                Cbor::Map(vec![(
                    text("form"),
                    Cbor::Map(vec![
                        (text("question"), text("")),
                        (text("options"), text("")),
                    ]),
                )]),
            );
        }
        "vote" => {
            let (Some(poll), Some(option)) = (
                resolve_poll(host, str_of(&payload, "poll").unwrap_or("")),
                str_of(&payload, "option"),
            ) else {
                return;
            };
            let Some(option) = resolve_option(host, &poll, option) else {
                return;
            };
            if host
                .get(&[key(&crate::join(&["p", &poll, "closed"]))])
                .as_bool()
                .unwrap_or(false)
            {
                ui::log(host, "That poll is closed", "warning");
                return;
            }
            host.put(
                &[key(&crate::join(&[
                    "b",
                    &poll,
                    &crate::event_me(event),
                    &option,
                ]))],
                crate::value::int(crate::event_ts(event)),
            );
        }
        "close" => {
            if let Some(poll) = resolve_poll(host, str_of(&payload, "poll").unwrap_or("")) {
                host.put(
                    &[key(&crate::join(&["p", &poll, "closed"]))],
                    Cbor::Bool(true),
                );
            }
        }
        "remove_poll" => {
            if let Some(poll) = resolve_poll(host, str_of(&payload, "poll").unwrap_or("")) {
                delete_prefix(host, &crate::prefix(&["p", &poll]));
                delete_prefix(host, &crate::prefix(&["b", &poll]));
            }
        }
        _ => return,
    }
    // The document write makes the host call us back with `change`, which renders.
}

fn options(payload: &Cbor, signals: &Cbor) -> Vec<String> {
    if let Some(items) = field(payload, "options").and_then(Cbor::as_array) {
        let options: Vec<String> = items
            .iter()
            .map(|item| {
                item.as_text()
                    .map(str::to_owned)
                    .or_else(|| Some(str_of(item, "text").unwrap_or("").to_owned()))
                    .unwrap_or_default()
            })
            .filter(|s| !s.is_empty())
            .collect();
        if !options.is_empty() {
            return options;
        }
    }
    str_of(payload, "options")
        .or_else(|| crate::form_str(signals, "options"))
        .map(crate::words)
        .unwrap_or_default()
}

fn render(host: &Host, initial: bool) {
    let polls = state(host);
    let mut el = Vec::new();
    el.push(El::new("po", "column").set("gap", text("medium")));
    el.push(
        El::new("po-title", "text")
            .parent("po")
            .variant("h1")
            .text("Polls"),
    );
    el.push(El::new("po-form", "card").parent("po").title("New poll"));
    el.push(
        El::new("po-question", "input")
            .parent("po-form")
            .set("label", text("Question"))
            .bind("form.question"),
    );
    el.push(
        El::new("po-options", "input")
            .parent("po-form")
            .set("label", text("Options"))
            .placeholder("Lunch, Dinner, Takeaway")
            .bind("form.options"),
    );
    el.push(
        El::new("po-create", "button")
            .parent("po-form")
            .variant("primary")
            .label("Create poll")
            .on_press("create", Cbor::Null),
    );

    for poll in polls {
        let card = format!("p-{}", ui::path_id(&poll.id));
        el.push(El::new(&card, "card").parent("po").title(&poll.question));
        el.push(El::new(&format!("{card}-head"), "row").parent(&card));
        if poll.closed {
            el.push(
                El::new(&format!("{card}-closed"), "badge")
                    .parent(&format!("{card}-head"))
                    .variant("warning")
                    .text("closed"),
            );
        }
        el.push(
            El::new(&format!("{card}-close"), "button")
                .parent(&format!("{card}-head"))
                .variant("plain")
                .label("Close voting")
                .on_press("close", Cbor::Map(vec![(text("poll"), text(&poll.id))])),
        );
        el.push(
            El::new(&format!("{card}-del"), "button")
                .parent(&format!("{card}-head"))
                .variant("failure")
                .label("Remove")
                .on_press(
                    "remove_poll",
                    Cbor::Map(vec![(text("poll"), text(&poll.id))]),
                ),
        );
        el.push(
            El::new(&format!("{card}-options"), "list")
                .parent(&card)
                .empty("No options."),
        );
        for option in &poll.options {
            let row = format!("o-{}", ui::path_id(&format!("{}:{}", poll.id, option.id)));
            el.push(El::new(&row, "row").parent(&format!("{card}-options")));
            el.push(
                El::new(&format!("{row}-text"), "text")
                    .parent(&row)
                    .text(&option.text),
            );
            el.push(
                El::new(&format!("{row}-count"), "badge")
                    .parent(&row)
                    .variant("info")
                    .text(&format!("{}", option.voters.len())),
            );
            el.push(
                El::new(&format!("{row}-vote"), "button")
                    .parent(&row)
                    .variant("secondary")
                    .label("Vote")
                    .disabled(poll.closed)
                    .on_press(
                        "vote",
                        Cbor::Map(vec![
                            (text("poll"), text(&poll.id)),
                            (text("option"), text(&option.id)),
                        ]),
                    ),
            );
        }
    }

    if initial {
        ui::start(
            host,
            "Polls",
            Cbor::Map(vec![(
                text("form"),
                Cbor::Map(vec![
                    (text("question"), text("")),
                    (text("options"), text("")),
                ]),
            )]),
            el,
        );
    } else {
        ui::update(host, el);
    }
}

struct Poll {
    id: String,
    question: String,
    closed: bool,
    multi: bool,
    options: Vec<PollOption>,
}

struct PollOption {
    id: String,
    text: String,
    voters: Vec<String>,
}

fn state(host: &Host) -> Vec<Poll> {
    let mut polls: BTreeMap<String, Poll> = BTreeMap::new();
    let mut ballots: BTreeMap<String, BTreeMap<String, BTreeMap<String, i64>>> = BTreeMap::new();
    for flat in host.keys(&[]) {
        let parts: Vec<&str> = flat.split(crate::SEP).collect();
        let value = host.get(&[key(&flat)]);
        match parts.as_slice() {
            ["p", poll, field] => {
                let poll = polls.entry((*poll).to_owned()).or_insert_with(|| Poll {
                    id: (*poll).to_owned(),
                    question: String::new(),
                    closed: false,
                    multi: false,
                    options: Vec::new(),
                });
                match *field {
                    "question" => poll.question = value.as_text().unwrap_or("").to_owned(),
                    "closed" => poll.closed = value.as_bool().unwrap_or(false),
                    "multi" => poll.multi = value.as_bool().unwrap_or(false),
                    _ => {}
                }
            }
            ["p", poll, "o", option, field] => {
                let options = &mut polls
                    .entry((*poll).to_owned())
                    .or_insert_with(|| Poll {
                        id: (*poll).to_owned(),
                        question: String::new(),
                        closed: false,
                        multi: false,
                        options: Vec::new(),
                    })
                    .options;
                let at = options.iter().position(|o| o.id == *option);
                let option = match at {
                    Some(at) => &mut options[at],
                    None => {
                        options.push(PollOption {
                            id: (*option).to_owned(),
                            text: (*option).to_owned(),
                            voters: Vec::new(),
                        });
                        options.last_mut().unwrap()
                    }
                };
                if *field == "text" {
                    let fallback = option.id.clone();
                    option.text = value.as_text().unwrap_or(&fallback).to_owned();
                }
            }
            ["b", poll, voter, option] => {
                ballots
                    .entry((*poll).to_owned())
                    .or_default()
                    .entry((*voter).to_owned())
                    .or_default()
                    .insert((*option).to_owned(), as_i64(&value));
            }
            _ => {}
        }
    }

    let mut out = Vec::new();
    for (id, mut poll) in polls {
        let empty = BTreeMap::new();
        let votes = ballots.get(&id).unwrap_or(&empty);
        for option in &mut poll.options {
            for (voter, choices) in votes {
                let Some(&mine) = choices.get(&option.id) else {
                    continue;
                };
                let counts = if poll.multi {
                    true
                } else {
                    choices
                        .iter()
                        .max_by(|a, b| a.1.cmp(b.1).then_with(|| a.0.cmp(b.0)))
                        .is_some_and(|(o, ts)| o == &option.id && *ts == mine)
                };
                if counts {
                    option.voters.push(voter.clone());
                }
            }
            option.voters.sort();
        }
        out.push(poll);
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn resolve_poll(host: &Host, wanted: &str) -> Option<String> {
    if wanted.is_empty() {
        return None;
    }
    if !host
        .get(&[key(&crate::join(&["p", wanted, "question"]))])
        .is_null()
    {
        return Some(wanted.to_owned());
    }
    for flat in host.keys(&[]) {
        let parts: Vec<&str> = flat.split(crate::SEP).collect();
        if let ["p", id, "question"] = parts.as_slice() {
            if host.get(&[key(&flat)]).as_text() == Some(wanted) {
                return Some((*id).to_owned());
            }
        }
    }
    None
}

fn resolve_option(host: &Host, poll: &str, wanted: &str) -> Option<String> {
    if !host
        .get(&[key(&crate::join(&["p", poll, "o", wanted, "text"]))])
        .is_null()
    {
        return Some(wanted.to_owned());
    }
    for flat in host.keys(&[]) {
        let parts: Vec<&str> = flat.split(crate::SEP).collect();
        if let ["p", p, "o", id, "text"] = parts.as_slice() {
            if *p == poll && host.get(&[key(&flat)]).as_text() == Some(wanted) {
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
