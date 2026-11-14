//! Expenses: who paid what, split between people, and what settles it.
//!
//! Flat state:
//! - `currency`
//! - `e<US><id><US>what|amount|payer|ts`, `e<US><id><US>among<US><peer>` = true
//! - `s<US><id><US>from|to|amount|by|ts`
//!
//! Amounts are integer cents. Actions:
//! - `{name:"add_expense", payload:{what, amount, payer?, among?}}` (`among` is a list or a
//!   comma-separated string; omitted, it is everyone in the group)
//! - `{name:"remove_expense", payload:{expense}}`
//! - `{name:"settle", payload:{from, to, amount}}`
//! - `{name:"set_currency", payload:{currency}}`
//!
//! Values may also come from the form signals (`form.what`, `form.amount`, …).

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
    let me = crate::event_me(event);
    let id = crate::new_id(event);

    match name {
        "add_expense" => {
            let what = str_of(&payload, "what")
                .or_else(|| crate::form_str(&signals, "what"))
                .unwrap_or("")
                .trim()
                .to_owned();
            let amount = i64_of(&payload, "amount")
                .or_else(|| crate::form_i64(&signals, "amount"))
                .unwrap_or(0);
            if what.is_empty() || amount <= 0 {
                ui::log(
                    host,
                    "An expense needs a name and a positive amount",
                    "warning",
                );
                return;
            }
            let payer = str_of(&payload, "payer")
                .filter(|p| !p.is_empty())
                .map(str::to_owned)
                .or_else(|| {
                    crate::form_str(&signals, "payer")
                        .filter(|p| !p.is_empty())
                        .map(str::to_owned)
                })
                .unwrap_or(me);
            let among = people(host, &payload, &signals);
            for (field, value) in [
                ("what", text(&what)),
                ("amount", crate::value::int(amount)),
                ("payer", text(&payer)),
                ("ts", crate::value::int(crate::event_ts(event))),
            ] {
                host.put(&[key(&crate::join(&["e", &id, field]))], value);
            }
            for person in among {
                host.put(
                    &[key(&crate::join(&["e", &id, "among", &person]))],
                    Cbor::Bool(true),
                );
            }
            ui::signal(host, clear_form(&["what", "amount", "payer", "among"]));
        }
        "remove_expense" => {
            let Some(expense) = resolve_expense(host, str_of(&payload, "expense").unwrap_or(""))
            else {
                return;
            };
            delete_prefix(host, &crate::prefix(&["e", &expense]));
        }
        "settle" => {
            let (Some(from), Some(to), Some(amount)) = (
                str_of(&payload, "from"),
                str_of(&payload, "to"),
                i64_of(&payload, "amount"),
            ) else {
                return;
            };
            if amount <= 0 || from == to {
                return;
            }
            for (field, value) in [
                ("from", text(from)),
                ("to", text(to)),
                ("amount", crate::value::int(amount)),
                ("by", text(&me)),
                ("ts", crate::value::int(crate::event_ts(event))),
            ] {
                host.put(&[key(&crate::join(&["s", &id, field]))], value);
            }
        }
        "set_currency" => {
            let currency = str_of(&payload, "currency")
                .or_else(|| crate::form_str(&signals, "currency"))
                .unwrap_or("");
            host.put(&[key("currency")], text(currency));
        }
        _ => return,
    }
    // The document write makes the host call us back with `change`, which renders.
}

fn people(host: &Host, payload: &Cbor, signals: &Cbor) -> Vec<String> {
    if let Some(items) = field(payload, "among").and_then(Cbor::as_array) {
        let people: Vec<String> = items
            .iter()
            .filter_map(|p| p.as_text().map(str::to_owned))
            .collect();
        if !people.is_empty() {
            return people;
        }
    }
    let string = str_of(payload, "among")
        .or_else(|| crate::form_str(signals, "among"))
        .unwrap_or("");
    let people = crate::words(string);
    if !people.is_empty() {
        return people;
    }
    host.members().into_iter().map(|(id, _)| id).collect()
}

fn clear_form(fields: &[&str]) -> Cbor {
    Cbor::Map(vec![(
        text("form"),
        Cbor::Map(
            fields
                .iter()
                .map(|f| (text(*f), if *f == "amount" { Cbor::Null } else { text("") }))
                .collect(),
        ),
    )])
}

fn render(host: &Host, initial: bool) {
    let expenses = state(host);
    let mut el = Vec::new();
    el.push(El::new("ex", "column").set("gap", text("medium")));
    el.push(
        El::new("ex-title", "text")
            .parent("ex")
            .variant("h1")
            .text("Expenses"),
    );

    el.push(El::new("ex-form", "card").parent("ex").title("Add expense"));
    el.push(El::new("ex-form-row1", "row").parent("ex-form"));
    el.push(
        El::new("ex-what", "input")
            .parent("ex-form-row1")
            .set("label", text("What"))
            .bind("form.what"),
    );
    el.push(
        El::new("ex-amount", "input")
            .parent("ex-form-row1")
            .set("label", text("Amount (cents)"))
            .variant("number")
            .bind("form.amount"),
    );
    el.push(El::new("ex-form-row2", "row").parent("ex-form"));
    el.push(
        El::new("ex-payer", "input")
            .parent("ex-form-row2")
            .set("label", text("Paid by"))
            .placeholder("me")
            .bind("form.payer"),
    );
    el.push(
        El::new("ex-among", "input")
            .parent("ex-form-row2")
            .set("label", text("Split between"))
            .placeholder("everyone")
            .bind("form.among"),
    );
    el.push(
        El::new("ex-add", "button")
            .parent("ex-form")
            .variant("primary")
            .label("Add expense")
            .on_press("add_expense", Cbor::Null),
    );

    el.push(
        El::new("ex-list", "list")
            .parent("ex")
            .empty("No expenses yet."),
    );
    for expense in &expenses.expenses {
        let row = format!("e-{}", ui::path_id(&expense.id));
        el.push(El::new(&row, "row").parent("ex-list"));
        el.push(
            El::new(&format!("{row}-what"), "text")
                .parent(&row)
                .text(&expense.what),
        );
        el.push(
            El::new(&format!("{row}-amount"), "badge")
                .parent(&row)
                .text(&money(expense.amount)),
        );
        el.push(
            El::new(&format!("{row}-who"), "text")
                .parent(&row)
                .variant("caption")
                .text(&format!("{} · {}", expense.payer, expense.among.join(", "))),
        );
        el.push(
            El::new(&format!("{row}-del"), "button")
                .parent(&row)
                .variant("plain")
                .label("Remove")
                .on_press(
                    "remove_expense",
                    Cbor::Map(vec![(text("expense"), text(&expense.id))]),
                ),
        );
    }

    el.push(El::new("ex-balances", "card").parent("ex").title(&format!(
        "Balances ({})",
        host.get(&[key("currency")]).as_text().unwrap_or("")
    )));
    for (person, balance) in &expenses.balances {
        el.push(
            El::new(&format!("b-{}", ui::path_id(person)), "text")
                .parent("ex-balances")
                .text(&format!("{person}: {}", signed_money(*balance))),
        );
    }

    el.push(El::new("ex-settle", "card").parent("ex").title("Settle up"));
    if expenses.transfers.is_empty() {
        el.push(
            El::new("ex-settled", "text")
                .parent("ex-settle")
                .variant("caption")
                .text("All square."),
        );
    }
    for transfer in &expenses.transfers {
        let row = format!(
            "s-{}",
            ui::path_id(&format!("{}:{}", transfer.from, transfer.to))
        );
        el.push(El::new(&row, "row").parent("ex-settle"));
        el.push(
            El::new(&format!("{row}-text"), "text")
                .parent(&row)
                .text(&format!(
                    "{} pays {} {}",
                    transfer.from,
                    transfer.to,
                    money(transfer.amount)
                )),
        );
        el.push(
            El::new(&format!("{row}-do"), "button")
                .parent(&row)
                .variant("secondary")
                .label("Settle")
                .on_press(
                    "settle",
                    Cbor::Map(vec![
                        (text("from"), text(&transfer.from)),
                        (text("to"), text(&transfer.to)),
                        (text("amount"), crate::value::int(transfer.amount)),
                    ]),
                ),
        );
    }

    if initial {
        ui::start(
            host,
            "Expenses",
            Cbor::Map(vec![(
                text("form"),
                Cbor::Map(vec![
                    (text("what"), text("")),
                    (text("payer"), text("")),
                    (text("among"), text("")),
                ]),
            )]),
            el,
        );
    } else {
        ui::update(host, el);
    }
}

fn signed_money(cents: i64) -> String {
    if cents > 0 {
        format!("+{}", money(cents))
    } else if cents < 0 {
        format!("-{}", money(-cents))
    } else {
        money(0)
    }
}

fn money(cents: i64) -> String {
    let cents = cents.abs();
    format!("{}.{:02}", cents / 100, cents % 100)
}

struct Transfer {
    from: String,
    to: String,
    amount: i64,
}

struct Expense {
    id: String,
    what: String,
    amount: i64,
    payer: String,
    among: Vec<String>,
}

#[derive(Default)]
struct State {
    expenses: Vec<Expense>,
    balances: BTreeMap<String, i64>,
    transfers: Vec<Transfer>,
}

fn state(host: &Host) -> State {
    let mut expenses: BTreeMap<String, Expense> = BTreeMap::new();
    let mut settlements: BTreeMap<String, (String, String, i64)> = BTreeMap::new();
    for flat in host.keys(&[]) {
        let parts: Vec<&str> = flat.split(crate::SEP).collect();
        let value = host.get(&[key(&flat)]);
        match parts.as_slice() {
            ["e", id, field] => {
                let expense = expenses.entry((*id).to_owned()).or_insert_with(|| Expense {
                    id: (*id).to_owned(),
                    what: String::new(),
                    amount: 0,
                    payer: String::new(),
                    among: Vec::new(),
                });
                match *field {
                    "what" => expense.what = value.as_text().unwrap_or("").to_owned(),
                    "amount" => expense.amount = as_i64(&value),
                    "payer" => expense.payer = value.as_text().unwrap_or("").to_owned(),
                    _ => {}
                }
            }
            ["e", id, "among", person] => {
                expenses
                    .entry((*id).to_owned())
                    .or_insert_with(|| Expense {
                        id: (*id).to_owned(),
                        what: String::new(),
                        amount: 0,
                        payer: String::new(),
                        among: Vec::new(),
                    })
                    .among
                    .push((*person).to_owned());
            }
            ["s", id, field] => {
                let settlement = settlements.entry((*id).to_owned()).or_default();
                match *field {
                    "from" => settlement.0 = value.as_text().unwrap_or("").to_owned(),
                    "to" => settlement.1 = value.as_text().unwrap_or("").to_owned(),
                    "amount" => settlement.2 = as_i64(&value),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    let mut balances: BTreeMap<String, i64> = BTreeMap::new();
    for expense in expenses.values_mut() {
        expense.among.sort();
        expense.among.dedup();
        if expense.amount <= 0 {
            continue;
        }
        let mut among: Vec<&str> = expense.among.iter().map(String::as_str).collect();
        if among.is_empty() {
            among.push(&expense.payer);
        }
        *balances.entry(expense.payer.clone()).or_default() += expense.amount;
        let n = among.len() as i64;
        let base = expense.amount / n;
        let rest = expense.amount % n;
        for (i, person) in among.iter().enumerate() {
            let share = base + i64::from((i as i64) < rest);
            *balances.entry((*person).to_owned()).or_default() -= share;
        }
    }
    for (from, to, amount) in settlements.values() {
        if *amount <= 0 {
            continue;
        }
        *balances.entry(from.clone()).or_default() += amount;
        *balances.entry(to.clone()).or_default() -= amount;
    }

    let mut debtors: Vec<(&str, i64)> = balances
        .iter()
        .filter(|(_, b)| **b < 0)
        .map(|(p, b)| (p.as_str(), -*b))
        .collect();
    let mut creditors: Vec<(&str, i64)> = balances
        .iter()
        .filter(|(_, b)| **b > 0)
        .map(|(p, b)| (p.as_str(), *b))
        .collect();
    debtors.sort_by_key(|(p, _)| *p);
    creditors.sort_by_key(|(p, _)| *p);
    let mut transfers = Vec::new();
    let mut c = 0;
    for (debtor, mut debt) in debtors {
        while debt > 0 && c < creditors.len() {
            let (creditor, credit) = creditors[c];
            let amount = debt.min(credit);
            transfers.push(Transfer {
                from: debtor.to_owned(),
                to: creditor.to_owned(),
                amount,
            });
            debt -= amount;
            creditors[c].1 -= amount;
            if creditors[c].1 == 0 {
                c += 1;
            }
        }
    }

    let mut expenses: Vec<Expense> = expenses.into_values().collect();
    expenses.sort_by(|a, b| a.id.cmp(&b.id));
    State {
        expenses,
        balances,
        transfers,
    }
}

fn resolve_expense(host: &Host, wanted: &str) -> Option<String> {
    if wanted.is_empty() {
        return None;
    }
    if !host
        .get(&[key(&crate::join(&["e", wanted, "amount"]))])
        .is_null()
    {
        return Some(wanted.to_owned());
    }
    for flat in host.keys(&[]) {
        let parts: Vec<&str> = flat.split(crate::SEP).collect();
        if let ["e", id, "what"] = parts.as_slice() {
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
