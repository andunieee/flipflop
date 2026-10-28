//! CBOR to and from the document: values, paths and patches, as described in [`super`].

use anyhow::{Result, bail};
use automerge::{
    AutoCommit, ObjId, ObjType, Patch, PatchAction, Prop, ReadDoc, ScalarValue, Value,
    transaction::Transactable,
};
use ciborium::Value as Cbor;

/// A text object (one people edit together), as opposed to a plain string: the tag around a string.
pub const TAG_TEXT: u64 = 0xF1F0;
/// A counter: the tag around an integer.
pub const TAG_COUNTER: u64 = 0xF1F1;
/// A timestamp: the tag around milliseconds since the Unix epoch.
pub const TAG_TIMESTAMP: u64 = 0xF1F2;

pub fn encode(value: &Cbor) -> Vec<u8> {
    let mut out = Vec::new();
    ciborium::into_writer(value, &mut out).expect("CBOR into a Vec");
    out
}

pub fn text(s: impl Into<String>) -> Cbor {
    Cbor::Text(s.into())
}

pub fn map(entries: impl IntoIterator<Item = (&'static str, Cbor)>) -> Cbor {
    Cbor::Map(entries.into_iter().map(|(k, v)| (text(k), v)).collect())
}

fn int(v: &Cbor) -> Result<i64> {
    match v {
        Cbor::Integer(i) => Ok(i64::try_from(i128::from(*i))?),
        _ => bail!("expected an integer"),
    }
}

// ---- reading ------------------------------------------------------------------------

/// An object and everything in it.
pub fn read_obj(doc: &AutoCommit, obj: &ObjId) -> Cbor {
    match doc.object_type(obj) {
        Ok(ObjType::Map | ObjType::Table) => Cbor::Map(
            doc.keys(obj)
                .map(|k| {
                    let v = read_prop(doc, obj, k.as_str());
                    (Cbor::Text(k), v)
                })
                .collect(),
        ),
        Ok(ObjType::List) => Cbor::Array(
            (0..doc.length(obj))
                .map(|i| read_prop(doc, obj, i))
                .collect(),
        ),
        Ok(ObjType::Text) => Cbor::Tag(TAG_TEXT, Box::new(text(doc.text(obj).unwrap_or_default()))),
        Err(_) => Cbor::Null,
    }
}

/// The value under `prop` of `obj`, null if there is none.
pub fn read_prop(doc: &AutoCommit, obj: &ObjId, prop: impl Into<Prop>) -> Cbor {
    match doc.get(obj, prop) {
        Ok(Some((Value::Object(_), id))) => read_obj(doc, &id),
        Ok(Some((Value::Scalar(s), _))) => scalar(&s),
        _ => Cbor::Null,
    }
}

pub fn scalar(s: &ScalarValue) -> Cbor {
    match s {
        ScalarValue::Bytes(b) => Cbor::Bytes(b.clone()),
        ScalarValue::Str(s) => text(s.as_str()),
        ScalarValue::Int(i) => Cbor::Integer((*i).into()),
        ScalarValue::Uint(u) => Cbor::Integer((*u).into()),
        ScalarValue::F64(f) => Cbor::Float(*f),
        ScalarValue::Counter(c) => {
            Cbor::Tag(TAG_COUNTER, Box::new(Cbor::Integer(i64::from(c).into())))
        }
        ScalarValue::Timestamp(t) => Cbor::Tag(TAG_TIMESTAMP, Box::new(Cbor::Integer((*t).into()))),
        ScalarValue::Boolean(b) => Cbor::Bool(*b),
        ScalarValue::Unknown { .. } | ScalarValue::Null => Cbor::Null,
    }
}

/// A value as a patch carries it: an object is new, so empty (later patches fill it).
fn patch_value(v: &Value) -> Cbor {
    match v {
        Value::Object(ObjType::Map | ObjType::Table) => Cbor::Map(vec![]),
        Value::Object(ObjType::List) => Cbor::Array(vec![]),
        Value::Object(ObjType::Text) => Cbor::Tag(TAG_TEXT, Box::new(text(""))),
        Value::Scalar(s) => scalar(s),
    }
}

// ---- writing ------------------------------------------------------------------------

enum New {
    Scalar(ScalarValue),
    /// The type, and the contents to fill it with.
    Object(ObjType, Cbor),
}

fn new(v: Cbor) -> Result<New> {
    Ok(match v {
        Cbor::Map(_) => New::Object(ObjType::Map, v),
        Cbor::Array(_) => New::Object(ObjType::List, v),
        Cbor::Tag(TAG_TEXT, inner) => match *inner {
            Cbor::Text(_) => New::Object(ObjType::Text, *inner),
            _ => bail!("a text is the tag around a string"),
        },
        Cbor::Tag(TAG_COUNTER, inner) => New::Scalar(ScalarValue::counter(int(&inner)?)),
        Cbor::Tag(TAG_TIMESTAMP, inner) => New::Scalar(ScalarValue::Timestamp(int(&inner)?)),
        // Tags we do not know of (a CBOR library's date, say): the value inside.
        Cbor::Tag(_, inner) => new(*inner)?,
        Cbor::Text(s) => New::Scalar(ScalarValue::from(s)),
        Cbor::Integer(i) => New::Scalar(match i64::try_from(i128::from(i)) {
            Ok(i) => ScalarValue::Int(i),
            Err(_) => ScalarValue::Uint(u64::try_from(i128::from(i))?),
        }),
        Cbor::Float(f) => New::Scalar(ScalarValue::F64(f)),
        Cbor::Bool(b) => New::Scalar(ScalarValue::Boolean(b)),
        Cbor::Null => New::Scalar(ScalarValue::Null),
        Cbor::Bytes(b) => New::Scalar(ScalarValue::Bytes(b)),
        _ => bail!("unsupported value"),
    })
}

/// Set `prop` of `parent` (a map key, or an index the list already has) to `value`.
pub fn put(doc: &mut AutoCommit, parent: &ObjId, prop: Prop, value: Cbor) -> Result<()> {
    match new(value)? {
        New::Scalar(s) => doc.put(parent, prop, s)?,
        New::Object(kind, contents) => {
            let obj = doc.put_object(parent, prop, kind)?;
            fill(doc, &obj, contents)?;
        }
    }
    Ok(())
}

/// Insert `value` into the list `parent` at `index` (its length appends).
pub fn insert(doc: &mut AutoCommit, parent: &ObjId, index: usize, value: Cbor) -> Result<()> {
    match new(value)? {
        New::Scalar(s) => doc.insert(parent, index, s)?,
        New::Object(kind, contents) => {
            let obj = doc.insert_object(parent, index, kind)?;
            fill(doc, &obj, contents)?;
        }
    }
    Ok(())
}

fn fill(doc: &mut AutoCommit, obj: &ObjId, contents: Cbor) -> Result<()> {
    match contents {
        Cbor::Map(entries) => {
            for (k, v) in entries {
                let Cbor::Text(k) = k else {
                    bail!("map keys must be strings")
                };
                put(doc, obj, Prop::Map(k), v)?;
            }
        }
        Cbor::Array(items) => {
            for (i, v) in items.into_iter().enumerate() {
                insert(doc, obj, i, v)?;
            }
        }
        Cbor::Text(t) => doc.splice_text(obj, 0, 0, &t)?,
        _ => {}
    }
    Ok(())
}

// ---- paths --------------------------------------------------------------------------

pub fn path(v: &Cbor) -> Result<Vec<Prop>> {
    let Cbor::Array(items) = v else {
        bail!("a path is an array")
    };
    items
        .iter()
        .map(|p| match p {
            Cbor::Text(k) => Ok(Prop::Map(k.clone())),
            Cbor::Integer(_) => Ok(Prop::Seq(usize::try_from(int(p)?)?)),
            _ => bail!("path steps are strings (map keys) or unsigned integers (list indices)"),
        })
        .collect()
}

fn prop(p: &Prop) -> Cbor {
    match p {
        Prop::Map(k) => text(k.as_str()),
        Prop::Seq(i) => Cbor::Integer((*i as u64).into()),
    }
}

/// The object at `path` below `from`.
pub fn resolve(doc: &AutoCommit, from: &ObjId, path: &[Prop]) -> Result<ObjId> {
    let mut obj = from.clone();
    for p in path {
        match doc.get(&obj, p.clone())? {
            Some((Value::Object(_), id)) => obj = id,
            _ => bail!("nothing to step into at {p:?}"),
        }
    }
    Ok(obj)
}

/// The object holding the last step of `path` below `from`, and that step.
pub fn split(doc: &AutoCommit, from: &ObjId, path: &[Prop]) -> Result<(ObjId, Prop)> {
    let Some((last, parents)) = path.split_last() else {
        bail!("the path is empty")
    };
    Ok((resolve(doc, from, parents)?, last.clone()))
}

// ---- patches ------------------------------------------------------------------------

/// One change to the document, before it is placed relative to what the app sees.
pub struct Change {
    /// From the root: the value that changed, or for splices and marks the text.
    pub path: Vec<Prop>,
    fields: Vec<(&'static str, Cbor)>,
}

impl Change {
    pub fn from_patch(p: &Patch) -> Option<Change> {
        let mut path: Vec<Prop> = p.path.iter().map(|(_, prop)| prop.clone()).collect();
        let mut at = |prop: Prop| path.push(prop);
        let fields = match &p.action {
            PatchAction::PutMap { key, value, .. } => {
                at(Prop::Map(key.clone()));
                vec![("action", text("put")), ("value", patch_value(&value.0))]
            }
            PatchAction::PutSeq { index, value, .. } => {
                at(Prop::Seq(*index));
                vec![("action", text("put")), ("value", patch_value(&value.0))]
            }
            PatchAction::Insert { index, values } => {
                at(Prop::Seq(*index));
                let values = values.iter().map(|(v, _, _)| patch_value(v)).collect();
                vec![("action", text("insert")), ("values", Cbor::Array(values))]
            }
            PatchAction::SpliceText { index, value, .. } => vec![
                ("action", text("splice")),
                ("index", Cbor::Integer((*index as u64).into())),
                ("text", text(value.make_string())),
            ],
            PatchAction::Increment { prop, value } => {
                at(prop.clone());
                vec![
                    ("action", text("increment")),
                    ("by", Cbor::Integer((*value).into())),
                ]
            }
            PatchAction::DeleteMap { key } => {
                at(Prop::Map(key.clone()));
                vec![("action", text("delete"))]
            }
            PatchAction::DeleteSeq { index, length } => {
                at(Prop::Seq(*index));
                vec![
                    ("action", text("delete")),
                    ("count", Cbor::Integer((*length as u64).into())),
                ]
            }
            // Conflicts resolve themselves (a put follows); apps write no marks.
            PatchAction::Conflict { .. } | PatchAction::Mark { .. } => return None,
        };
        Some(Change { path, fields })
    }

    /// The patch as the app gets it, with the path made relative by dropping `skip` steps.
    pub fn encode(self, root: &'static str, skip: usize) -> Cbor {
        let path = Cbor::Array(self.path[skip..].iter().map(prop).collect());
        map([("root", text(root)), ("path", path)]
            .into_iter()
            .chain(self.fields))
    }
}
