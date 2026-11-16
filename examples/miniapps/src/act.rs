//! An `action` event, read the way apps need it.

use ciborium::Value as Cbor;

use crate::value::{self, field, str_of};

pub struct Act {
    pub name: String,
    /// The peer who did it.
    pub me: String,
    /// An id for what this action creates: unique to this device and action.
    pub id: String,
    pub ts: i64,
    payload: Cbor,
    signals: Cbor,
}

impl Act {
    pub fn parse(event: &Cbor) -> Act {
        Act {
            name: str_of(event, "name").unwrap_or("").to_owned(),
            me: crate::event_me(event),
            id: crate::new_id(event),
            ts: crate::event_ts(event),
            payload: crate::payload(event),
            signals: crate::signals(event),
        }
    }

    /// A string of the button's payload.
    pub fn arg(&self, key: &str) -> &str {
        str_of(&self.payload, key).unwrap_or("")
    }

    /// The signal at `path`, such as `["form", "title"]`.
    pub fn signal(&self, path: &[&str]) -> Option<&Cbor> {
        let mut at = &self.signals;
        for step in path {
            at = field(at, step)?;
        }
        Some(at)
    }

    /// What the user typed in `form.<key>`, trimmed.
    pub fn form(&self, key: &str) -> &str {
        self.signal(&["form", key])
            .and_then(Cbor::as_text)
            .unwrap_or("")
            .trim()
    }

    /// A number input's value, or a number typed as text (`12.50`, `12,5`).
    pub fn form_num(&self, key: &str) -> Option<f64> {
        match self.signal(&["form", key])? {
            Cbor::Integer(i) => i128::from(*i).to_string().parse().ok(),
            Cbor::Float(f) => Some(*f),
            Cbor::Text(t) => t.trim().replace(',', ".").parse().ok(),
            _ => None,
        }
    }

    pub fn form_bool(&self, path: &[&str]) -> Option<bool> {
        self.signal(path)?.as_bool()
    }

    /// A string in the `sync` signals: what a bound control said.
    pub fn sync_str(&self, path: &[&str]) -> &str {
        let mut full = vec!["sync"];
        full.extend_from_slice(path);
        self.signal(&full).and_then(Cbor::as_text).unwrap_or("")
    }

    pub fn sync_bool(&self, path: &[&str]) -> Option<bool> {
        let mut full = vec!["sync"];
        full.extend_from_slice(path);
        self.signal(&full)?.as_bool()
    }
}

/// `{"form": {<key>: <value>, …}}`, to set the user's inputs.
pub fn form_patch(fields: &[(&'static str, Cbor)]) -> Cbor {
    value::map([(
        "form",
        Cbor::Map(
            fields
                .iter()
                .map(|(k, v)| (value::text(*k), v.clone()))
                .collect(),
        ),
    )])
}

pub fn payload(pairs: &[(&'static str, &str)]) -> Cbor {
    value::map(pairs.iter().map(|(k, v)| (*k, value::text(*v))))
}
