//! Expenses: who paid what, split between people, and who owes whom.
//!
//! Amounts are typed as decimals (`12.50`) and kept as integer cents; the currency is a label.
//!
//! Flat state:
//! - `currency`
//! - `e<US><id><US>what|amount|payer|ts`, `e<US><id><US>among<US><peer>` = true
//! - `s<US><id><US>from|to|amount|ts`: a payment between two people that settles part of what is owed
//!
//! Actions:
//! - `add_expense` (`form.what|amount|payer`, `form.among.<peer hash>` ticks)
//! - `remove_expense {expense}` (asks twice), `settle {from, to, amount}`, `remove_settlement {id}`
//! - `set_currency` (`sync.currency`)

use std::collections::BTreeMap;

use ciborium::Value as Cbor;

use crate::act::{form_patch, payload, Act};
use crate::doc::{Doc, People};
use crate::ui::{self, badge, button, card, input, row, text, El, Page};
use crate::value::{int, key, map, text as cbor_text};
use crate::Host;

pub const NAME: &str = "Expenses";

pub fn title() -> String {
    NAME.to_owned()
}

/// Nothing to ask: the app starts empty.
pub fn params() -> Cbor {
    crate::params(Vec::new())
}

const CURRENCIES: [&str; 8] = ["EUR", "USD", "GBP", "BRL", "CAD", "AUD", "CHF", "SEK"];

pub fn form(host: &Host) -> Cbor {
    let people = People::load(host);
    map([
        ("what", cbor_text("")),
        ("amount", Cbor::Null),
        ("payer", cbor_text(&crate::me())),
        (
            "among",
            Cbor::Map(
                people
                    .ids()
                    .iter()
                    .map(|id| (cbor_text(ui::path_id(id)), Cbor::Bool(true)))
                    .collect(),
            ),
        ),
    ])
}

pub fn action(host: &Host, event: &Cbor) -> Result<(), String> {
    let act = Act::parse(event);
    let doc = Doc::load(host);
    let people = People::load(host);
    match act.name.as_str() {
        "add_expense" => {
            let what = act.form("what");
            if what.is_empty() {
                return Err("What was it for?".into());
            }
            let amount = act
                .form_num("amount")
                .filter(|a| a.is_finite() && *a > 0.0)
                .ok_or("Type how much it cost, like 12.50.")?;
            let cents = (amount * 100.0).round() as i64;
            let payer = match act.form("payer") {
                "" => act.me.clone(),
                payer => payer.to_owned(),
            };
            let among: Vec<String> = people
                .ids()
                .into_iter()
                .filter(|id| act.form_bool(&["form", "among", &ui::path_id(id)]) == Some(true))
                .collect();
            if among.is_empty() {
                return Err("Tick at least one person to split it with.".into());
            }
            put(host, &["e", &act.id, "what"], cbor_text(what));
            put(host, &["e", &act.id, "amount"], int(cents));
            put(host, &["e", &act.id, "payer"], cbor_text(&payer));
            put(host, &["e", &act.id, "ts"], int(act.ts));
            for peer in &among {
                put(host, &["e", &act.id, "among", peer], Cbor::Bool(true));
            }
            ui::signal(
                host,
                form_patch(&[("what", cbor_text("")), ("amount", Cbor::Null)]),
            );
        }
        "remove_expense" => {
            let expense = act.arg("expense");
            if !ui::confirmed(&format!("remove {expense}")) {
                return Ok(());
            }
            for flat in doc.keys_of(&["e", expense]) {
                host.delete(&[key(&flat)]);
            }
        }
        "settle" => {
            let cents: i64 = act
                .arg("amount")
                .parse()
                .map_err(|_| "That is not an amount.")?;
            if cents <= 0 {
                return Ok(());
            }
            put(host, &["s", &act.id, "from"], cbor_text(act.arg("from")));
            put(host, &["s", &act.id, "to"], cbor_text(act.arg("to")));
            put(host, &["s", &act.id, "amount"], int(cents));
            put(host, &["s", &act.id, "ts"], int(act.ts));
        }
        "remove_settlement" => {
            for flat in doc.keys_of(&["s", act.arg("id")]) {
                host.delete(&[key(&flat)]);
            }
        }
        "set_currency" => {
            let currency = act.sync_str(&["currency"]);
            if CURRENCIES.contains(&currency) {
                host.put(&[key("currency")], cbor_text(currency));
            }
        }
        _ => {}
    }
    Ok(())
}

fn put(host: &Host, parts: &[&str], value: Cbor) {
    host.put(&[key(&crate::join(parts))], value);
}

struct Expense {
    id: String,
    what: String,
    cents: i64,
    payer: String,
    ts: i64,
    among: Vec<String>,
}

struct Payment {
    id: String,
    from: String,
    to: String,
    cents: i64,
    ts: i64,
}

fn expenses(doc: &Doc) -> Vec<Expense> {
    doc.ids("e")
        .into_iter()
        .filter(|id| doc.get(&["e", id, "amount"]).is_some())
        .map(|id| Expense {
            what: doc.str(&["e", &id, "what"]).to_owned(),
            cents: doc.int(&["e", &id, "amount"]),
            payer: doc.str(&["e", &id, "payer"]).to_owned(),
            ts: doc.int(&["e", &id, "ts"]),
            among: doc
                .under(&["e", &id, "among"])
                .into_iter()
                .filter(|(_, v)| v.as_bool() == Some(true))
                .map(|(p, _)| p[0].to_owned())
                .collect(),
            id,
        })
        .collect()
}

fn payments(doc: &Doc) -> Vec<Payment> {
    doc.ids("s")
        .into_iter()
        .filter(|id| doc.get(&["s", id, "amount"]).is_some())
        .map(|id| Payment {
            from: doc.str(&["s", &id, "from"]).to_owned(),
            to: doc.str(&["s", &id, "to"]).to_owned(),
            cents: doc.int(&["s", &id, "amount"]),
            ts: doc.int(&["s", &id, "ts"]),
            id,
        })
        .collect()
}

/// What each person is owed (positive) or owes (negative), in cents.
fn balances(expenses: &[Expense], payments: &[Payment]) -> BTreeMap<String, i64> {
    let mut balance: BTreeMap<String, i64> = BTreeMap::new();
    for expense in expenses {
        *balance.entry(expense.payer.clone()).or_default() += expense.cents;
        let mut among = expense.among.clone();
        among.sort();
        let n = among.len() as i64;
        if n == 0 {
            continue;
        }
        // The cents that do not divide evenly go to the first few, the same on every device.
        for (i, person) in among.iter().enumerate() {
            let share = expense.cents / n + i64::from((i as i64) < expense.cents % n);
            *balance.entry(person.clone()).or_default() -= share;
        }
    }
    for payment in payments {
        *balance.entry(payment.from.clone()).or_default() += payment.cents;
        *balance.entry(payment.to.clone()).or_default() -= payment.cents;
    }
    balance
}

/// A few payments that settle everything: the biggest debtor pays the biggest creditor.
fn transfers(balance: &BTreeMap<String, i64>) -> Vec<(String, String, i64)> {
    let mut owed: Vec<(String, i64)> = balance
        .iter()
        .filter(|(_, c)| **c > 0)
        .map(|(p, c)| (p.clone(), *c))
        .collect();
    let mut owes: Vec<(String, i64)> = balance
        .iter()
        .filter(|(_, c)| **c < 0)
        .map(|(p, c)| (p.clone(), -*c))
        .collect();
    let mut out = Vec::new();
    while !owed.is_empty() && !owes.is_empty() {
        owed.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        owes.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let pay = owed[0].1.min(owes[0].1);
        out.push((owes[0].0.clone(), owed[0].0.clone(), pay));
        owed[0].1 -= pay;
        owes[0].1 -= pay;
        owed.retain(|(_, c)| *c > 0);
        owes.retain(|(_, c)| *c > 0);
    }
    out
}

fn money(cents: i64, currency: &str) -> String {
    format!("{}.{:02} {currency}", cents.abs() / 100, cents.abs() % 100)
}

pub fn page(host: &Host) -> Page {
    let doc = Doc::load(host);
    let people = People::load(host);
    let currency = match doc.str(&["currency"]) {
        "" => "EUR".to_owned(),
        c => c.to_owned(),
    };
    let expenses = expenses(&doc);
    let payments = payments(&doc);
    let balance = balances(&expenses, &payments);
    let mut page = Page::new(NAME);

    page.add(row("cur", "root").set("justify", cbor_text("end")));
    page.add(
        El::new("cur-pick", "select")
            .parent("cur")
            .label("Currency")
            .bind("sync.currency")
            .set(
                "options",
                Cbor::Array(
                    CURRENCIES
                        .iter()
                        .map(|c| map([("value", cbor_text(*c)), ("label", cbor_text(*c))]))
                        .collect(),
                ),
            )
            .on_change("set_currency", Cbor::Null),
    );
    page.sync("currency", cbor_text(&currency));

    page.add(card("new", "root", ""));
    page.add(input("new-what", "new", "What was it for?", "form.what").placeholder("Groceries"));
    page.add(row("new-pay", "new"));
    page.add(
        input(
            "new-amount",
            "new-pay",
            &format!("Amount ({currency})"),
            "form.amount",
        )
        .variant("number")
        .placeholder("12.50"),
    );
    page.add(
        El::new("new-payer", "select")
            .parent("new-pay")
            .label("Paid by")
            .bind("form.payer")
            .set(
                "options",
                Cbor::Array(
                    people
                        .ids()
                        .iter()
                        .map(|id| {
                            map([
                                ("value", cbor_text(id)),
                                ("label", cbor_text(people.name(id))),
                            ])
                        })
                        .collect(),
                ),
            ),
    );
    page.add(text("new-split", "new", "caption", "Split between"));
    page.add(row("new-among", "new").set("wrap", Cbor::Bool(true)));
    for id in people.ids() {
        page.add(
            El::new(&format!("new-among-{}", ui::path_id(&id)), "checkbox")
                .parent("new-among")
                .label(&people.name(&id))
                .bind(&format!("form.among.{}", ui::path_id(&id))),
        );
    }
    page.add(button(
        "new-go",
        "new",
        "primary",
        "+",
        "add_expense",
        Cbor::Null,
    ));

    // Balances and what would settle them.
    let settle = !(expenses.is_empty() && payments.is_empty());
    if settle {
        page.add(card("bal", "root", "Who owes whom"));
    }
    for (person, cents) in balance.iter().filter(|_| settle) {
        let line = format!("bal-{}", ui::path_id(person));
        page.add(row(&line, "bal"));
        page.add(text(
            &format!("{line}-name"),
            &line,
            "body",
            &people.name(person),
        ));
        let (variant, label) = match cents {
            0 => ("neutral", "settled".to_owned()),
            c if *c > 0 => ("success", format!("is owed {}", money(*c, &currency))),
            c => ("failure", format!("owes {}", money(*c, &currency))),
        };
        page.add(badge(&format!("{line}-amount"), &line, variant, &label));
    }
    for (i, (from, to, cents)) in transfers(&balance).iter().enumerate() {
        let line = format!("pay{i}");
        page.add(row(&line, "bal"));
        page.add(text(
            &format!("{line}-t"),
            &line,
            "body",
            &format!(
                "{} pays {} {}",
                people.name(from),
                people.name(to),
                money(*cents, &currency)
            ),
        ));
        page.add(button(
            &format!("{line}-go"),
            &line,
            "secondary",
            "Mark paid",
            "settle",
            payload(&[("from", from), ("to", to), ("amount", &cents.to_string())]),
        ));
    }

    // Newest first: expenses and payments in one history.
    enum Entry<'a> {
        Expense(&'a Expense),
        Payment(&'a Payment),
    }
    let mut history: Vec<(i64, Entry)> = expenses
        .iter()
        .map(|e| (e.ts, Entry::Expense(e)))
        .chain(payments.iter().map(|p| (p.ts, Entry::Payment(p))))
        .collect();
    history.sort_by_key(|(ts, _)| std::cmp::Reverse(*ts));
    if !history.is_empty() {
        page.add(card("hist", "root", "History"));
    }
    for (ts, entry) in &history {
        let day = crate::time::day_label(crate::time::day_of(*ts));
        match entry {
            Entry::Expense(e) => {
                let line = format!("h-{}", ui::path_id(&e.id));
                page.add(row(&line, "hist"));
                page.add(
                    El::new(&format!("{line}-t"), "column")
                        .parent(&line)
                        .set("gap", cbor_text("none")),
                );
                page.add(text(
                    &format!("{line}-what"),
                    &format!("{line}-t"),
                    "body",
                    &format!("{} · {}", e.what, money(e.cents, &currency)),
                ));
                page.add(text(
                    &format!("{line}-who"),
                    &format!("{line}-t"),
                    "caption",
                    &format!(
                        "{} paid · split between {} · {day}",
                        people.name(&e.payer),
                        people.list(&e.among)
                    ),
                ));
                let asking = ui::asking(&format!("remove {}", e.id));
                page.add(button(
                    &format!("{line}-del"),
                    &line,
                    if asking { "failure" } else { "plain" },
                    if asking { "Press again" } else { "Remove" },
                    "remove_expense",
                    payload(&[("expense", &e.id)]),
                ));
            }
            Entry::Payment(p) => {
                let line = format!("h-{}", ui::path_id(&p.id));
                page.add(row(&line, "hist"));
                page.add(text(
                    &format!("{line}-t"),
                    &line,
                    "caption",
                    &format!(
                        "{} paid {} {} · {day}",
                        people.name(&p.from),
                        people.name(&p.to),
                        money(p.cents, &currency)
                    ),
                ));
                page.add(button(
                    &format!("{line}-del"),
                    &line,
                    "plain",
                    "Undo",
                    "remove_settlement",
                    payload(&[("id", &p.id)]),
                ));
            }
        }
    }
    page
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expense(cents: i64, payer: &str, among: &[&str]) -> Expense {
        Expense {
            id: "e".into(),
            what: String::new(),
            cents,
            payer: payer.into(),
            ts: 0,
            among: among.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn uneven_splits_balance_to_zero() {
        let expenses = [
            expense(1000, "a", &["a", "b", "c"]),
            expense(1, "b", &["a", "b"]),
        ];
        let balance = balances(&expenses, &[]);
        assert_eq!(balance.values().sum::<i64>(), 0);
        let mut settled = balance.clone();
        for (from, to, cents) in transfers(&balance) {
            *settled.get_mut(&from).unwrap() += cents;
            *settled.get_mut(&to).unwrap() -= cents;
        }
        assert!(settled.values().all(|c| *c == 0), "{settled:?}");
    }
}
