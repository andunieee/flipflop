//! Reading the app's section of the document: one request for all of it, then lookups by flat key.
//! (A host call per key would be a call per row on every redraw.)

use std::collections::BTreeMap;

use ciborium::Value as Cbor;

use crate::{join, Host, SEP};

pub struct Doc(BTreeMap<String, Cbor>);

impl Doc {
    pub fn load(host: &Host) -> Doc {
        Doc(crate::value::text_entries(&host.get(&[]))
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.clone()))
            .collect())
    }

    pub fn get(&self, parts: &[&str]) -> Option<&Cbor> {
        self.0.get(&join(parts))
    }

    pub fn str(&self, parts: &[&str]) -> &str {
        self.get(parts).and_then(Cbor::as_text).unwrap_or("")
    }

    pub fn int(&self, parts: &[&str]) -> i64 {
        self.get(parts)
            .and_then(Cbor::as_integer)
            .and_then(|i| i64::try_from(i128::from(i)).ok())
            .unwrap_or(0)
    }

    pub fn bool(&self, parts: &[&str]) -> bool {
        self.get(parts).and_then(Cbor::as_bool).unwrap_or(false)
    }

    /// The ids of the things tagged `tag` (keys `<tag><US><id><US>…`), in order.
    pub fn ids(&self, tag: &str) -> Vec<String> {
        let prefix = format!("{tag}{SEP}");
        let mut ids: Vec<String> = Vec::new();
        for (key, _) in self.0.range(prefix.clone()..) {
            let Some(rest) = key.strip_prefix(&prefix) else {
                break;
            };
            let id = rest.split(SEP).next().unwrap_or("");
            if ids.last().map(String::as_str) != Some(id) {
                ids.push(id.to_owned());
            }
        }
        ids
    }

    /// What is under `parts`: the rest of each key's parts, and the value.
    pub fn under(&self, parts: &[&str]) -> Vec<(Vec<&str>, &Cbor)> {
        let prefix = crate::prefix(parts);
        self.0
            .range(prefix.clone()..)
            .take_while(|(k, _)| k.starts_with(&prefix))
            .map(|(k, v)| (k[prefix.len()..].split(SEP).collect(), v))
            .collect()
    }

    /// Every key of one thing, to delete it.
    pub fn keys_of(&self, parts: &[&str]) -> Vec<String> {
        let prefix = crate::prefix(parts);
        self.0
            .range(prefix.clone()..)
            .take_while(|(k, _)| k.starts_with(&prefix))
            .map(|(k, _)| k.clone())
            .collect()
    }
}

/// Peers' names, from the group document.
pub struct People {
    names: BTreeMap<String, String>,
}

impl People {
    pub fn load(host: &Host) -> People {
        People {
            names: host.members().into_iter().collect(),
        }
    }

    /// A peer's name, or the start of their id if the group does not name them.
    pub fn name(&self, id: &str) -> String {
        match self.names.get(id).filter(|n| !n.is_empty()) {
            Some(name) => name.clone(),
            None => id.chars().take(8).collect(),
        }
    }

    /// Every member's id, ordered by name.
    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<&String> = self.names.keys().collect();
        ids.sort_by_key(|id| self.name(id).to_lowercase());
        ids.into_iter().cloned().collect()
    }

    /// Names in a list: "Ana, Bo and Cy".
    pub fn list(&self, ids: &[String]) -> String {
        let names: Vec<String> = ids.iter().map(|id| self.name(id)).collect();
        match names.as_slice() {
            [] => String::new(),
            [one] => one.clone(),
            [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
        }
    }
}
