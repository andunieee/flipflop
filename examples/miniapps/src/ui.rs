//! hypereffect frames: build a `vtree` of elements, diff it against the last one, and emit the
//! resulting `signal`/`element`/`remove`/`metadata` lines to the host's renderer.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use ciborium::Value as Cbor;
use hypereffect::vtree::Node;
use hypereffect::{json, vtree, Element, Handler, Metadata, ServerMessage, Value};

use crate::Host;

/// One element, before it is placed in the tree.
pub struct El(Element);

impl El {
    pub fn new(id: &str, kind: &str) -> El {
        El(Element::new(
            id,
            hypereffect::Component::from_name(kind)
                .unwrap_or_else(|| panic!("bad component {kind}")),
        ))
    }

    pub fn set(mut self, key: &str, value: Cbor) -> El {
        self.0 = self.0.set(key, ciborium_to_json(&value));
        self
    }

    pub fn parent(mut self, id: &str) -> El {
        self.0 = self.0.parent(id);
        self
    }

    pub fn text(self, value: &str) -> El {
        self.set("text", crate::value::text(value))
    }

    pub fn label(self, value: &str) -> El {
        self.set("label", crate::value::text(value))
    }

    pub fn variant(self, value: &str) -> El {
        self.set("variant", crate::value::text(value))
    }

    pub fn title(self, value: &str) -> El {
        self.set("title", crate::value::text(value))
    }

    pub fn placeholder(self, value: &str) -> El {
        self.set("placeholder", crate::value::text(value))
    }

    pub fn empty(self, value: &str) -> El {
        self.set("empty", crate::value::text(value))
    }

    pub fn bind(mut self, path: &str) -> El {
        self.0 = self.0.bind(path);
        self
    }

    pub fn visible(self, value: bool) -> El {
        self.set("visible", Cbor::Bool(value))
    }

    pub fn disabled(self, value: bool) -> El {
        self.set("disabled", Cbor::Bool(value))
    }

    pub fn on_press(mut self, action: &str, payload: Cbor) -> El {
        self.0 = self.0.on_press(handler(action, &payload));
        self
    }

    pub fn on_change(mut self, action: &str, payload: Cbor) -> El {
        self.0 = self.0.on_change(handler(action, &payload));
        self
    }

    pub fn on_submit(mut self, action: &str, payload: Cbor) -> El {
        self.0 = self.0.on_submit(handler(action, &payload));
        self
    }
}

fn handler(action: &str, payload: &Cbor) -> Handler {
    let mut handler = Handler::action(action);
    let payload = ciborium_to_json(payload);
    if !payload.is_null() {
        handler = handler.payload(payload);
    }
    handler
}

/// A stable signal path segment for an id (which may contain anything).
pub fn path_id(id: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("h{hash:016x}")
}

#[derive(Default)]
struct Ui {
    last: Vec<Node>,
}

static UI: OnceLock<Mutex<Ui>> = OnceLock::new();

fn ui() -> &'static Mutex<Ui> {
    UI.get_or_init(|| Mutex::new(Ui::default()))
}

/// A new session: the renderer starts with an empty tree.
pub fn reset() {
    *ui().lock().unwrap() = Ui::default();
}

/// Start the session: metadata, initial signals, then the whole tree.
pub fn start(host: &Host, name: &str, signals: Cbor, elements: Vec<El>) {
    let roots = tree(elements);
    let mut out = String::new();
    out.push_str(&ServerMessage::Metadata(Metadata::new().name(name)).to_line());
    out.push_str(&ServerMessage::signal(ciborium_to_json(&signals)).to_line());
    for message in vtree::render_into(None, &roots) {
        out.push_str(&message.to_line());
    }
    host.emit_text(&out);
    ui().lock().unwrap().last = roots;
}

/// Re-send what changed, add what is new, and remove what is gone.
pub fn update(host: &Host, elements: Vec<El>) {
    let roots = tree(elements);
    let mut ui = ui().lock().unwrap();
    let messages = vtree::diff_into(None, &ui.last, &roots);
    if messages.is_empty() {
        return;
    }
    let mut out = String::new();
    for message in &messages {
        out.push_str(&message.to_line());
    }
    host.emit_text(&out);
    ui.last = roots;
}

/// Send one signal patch (a JSON Merge Patch), for example to clear an input after an action.
pub fn signal(host: &Host, patch: Cbor) {
    host.emit_text(&ServerMessage::signal(ciborium_to_json(&patch)).to_line());
}

/// An app's diagnostic; hypereffect 0.6.1's server messages have no log line, so this goes to the
/// host's log instead.
pub fn log(host: &Host, summary: &str, _kind: &str) {
    host.log(summary);
}

/// Assemble the flat list (parents before children) into the `vtree` forest. The order of each
/// parent's children is the order they appear in the list.
fn tree(elements: Vec<El>) -> Vec<Node> {
    let elements: Vec<Element> = elements.into_iter().map(|e| e.0).collect();
    let by_id: HashMap<String, Element> =
        elements.iter().map(|e| (e.id.clone(), e.clone())).collect();
    let mut order: HashMap<String, Vec<String>> = HashMap::new();
    let mut roots = Vec::new();
    for element in &elements {
        match element.parent.as_ref().filter(|p| by_id.contains_key(*p)) {
            Some(parent) => order
                .entry(parent.clone())
                .or_default()
                .push(element.id.clone()),
            None => roots.push(element.id.clone()),
        }
    }
    let mut seen = HashSet::new();
    roots
        .into_iter()
        .filter_map(|id| build(&id, &by_id, &order, &mut seen))
        .collect()
}

fn build(
    id: &str,
    by_id: &HashMap<String, Element>,
    order: &HashMap<String, Vec<String>>,
    seen: &mut HashSet<String>,
) -> Option<Node> {
    if !seen.insert(id.to_owned()) {
        return None;
    }
    let element = by_id.get(id)?.clone();
    let children = order
        .get(id)
        .into_iter()
        .flatten()
        .filter_map(|child| build(child, by_id, order, seen))
        .collect();
    Some(Node { element, children })
}

/// ciborium values (the document side) to the JSON values elements and signals use.
pub fn ciborium_to_json(value: &Cbor) -> Value {
    match value {
        Cbor::Null => Value::Null,
        Cbor::Bool(b) => Value::Bool(*b),
        Cbor::Integer(i) => {
            let i = i128::from(*i);
            if let Ok(i) = i64::try_from(i) {
                json!(i)
            } else if let Ok(i) = u64::try_from(i) {
                json!(i)
            } else {
                Value::Null
            }
        }
        Cbor::Float(f) => json!(f),
        Cbor::Text(s) => Value::String(s.clone()),
        Cbor::Array(items) => Value::Array(items.iter().map(ciborium_to_json).collect()),
        Cbor::Map(entries) => Value::Object(
            entries
                .iter()
                .filter_map(|(k, v)| Some((k.as_text()?.to_owned(), ciborium_to_json(v))))
                .collect(),
        ),
        Cbor::Bytes(bytes) => Value::String(format!("<{} bytes>", bytes.len())),
        Cbor::Tag(_, inner) => ciborium_to_json(inner),
        _ => Value::Null,
    }
}
