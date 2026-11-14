//! Small helpers over [`ciborium::Value`]: building maps and paths, reading fields, and putting a
//! value in one canonical order so two computed values can be compared.

use ciborium::Value as Cbor;

pub fn text(s: impl Into<String>) -> Cbor {
    Cbor::Text(s.into())
}

pub fn int(i: i64) -> Cbor {
    Cbor::Integer(i.into())
}

pub fn uint(i: usize) -> Cbor {
    Cbor::Integer((i as u64).into())
}

/// A map from string keys. Kept in the order given; [`canonical`] sorts it.
pub fn map(entries: impl IntoIterator<Item = (&'static str, Cbor)>) -> Cbor {
    Cbor::Map(entries.into_iter().map(|(k, v)| (text(k), v)).collect())
}

/// A path step for a map key.
pub fn key(k: &str) -> Cbor {
    text(k)
}

/// A path step for a list index.
pub fn at(i: usize) -> Cbor {
    uint(i)
}

pub fn path(parts: &[Cbor]) -> Cbor {
    Cbor::Array(parts.to_vec())
}

pub fn field<'a>(v: &'a Cbor, name: &str) -> Option<&'a Cbor> {
    v.as_map()?
        .iter()
        .find(|(k, _)| k.as_text() == Some(name))
        .map(|(_, v)| v)
}

pub fn str_of<'a>(v: &'a Cbor, name: &str) -> Option<&'a str> {
    field(v, name)?.as_text()
}

pub fn i64_of(v: &Cbor, name: &str) -> Option<i64> {
    field(v, name)?
        .as_integer()
        .and_then(|i| i64::try_from(i128::from(i)).ok())
}

pub fn bool_of(v: &Cbor, name: &str) -> Option<bool> {
    field(v, name)?.as_bool()
}

/// A map's entries, keyed by text. Other keys are skipped.
pub fn text_entries(v: &Cbor) -> Vec<(&str, &Cbor)> {
    v.as_map()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| Some((k.as_text()?, v)))
                .collect()
        })
        .unwrap_or_default()
}

/// The value with every map's keys sorted, so equal values compare equal however they were built.
pub fn canonical(v: Cbor) -> Cbor {
    match v {
        Cbor::Map(m) => {
            let mut m: Vec<(Cbor, Cbor)> = m.into_iter().map(|(k, v)| (k, canonical(v))).collect();
            m.sort_by(|a, b| sort_key(&a.0).cmp(&sort_key(&b.0)));
            Cbor::Map(m)
        }
        Cbor::Array(a) => Cbor::Array(a.into_iter().map(canonical).collect()),
        other => other,
    }
}

fn sort_key(v: &Cbor) -> String {
    match v {
        Cbor::Text(s) => s.clone(),
        other => format!("{other:?}"),
    }
}
