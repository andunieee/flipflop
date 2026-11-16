//! hypereffect frames: build a `vtree` of elements, diff it against the last one, and emit the
//! resulting `signal`/`element`/`remove`/`metadata` lines to the host's renderer.

use std::collections::{BTreeMap, HashMap, HashSet};
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

/// One screen: the elements (parents before their children, siblings in order) and the signals the
/// app owns. The `sync` signals mirror the document (a checkbox's state, a selected answer) and the
/// app sets them on every redraw; the `form` signals belong to the user's inputs.
pub struct Page {
    els: Vec<El>,
    sync: BTreeMap<String, Cbor>,
}

impl Page {
    /// A page with its title, and the app's last error as a notice under it.
    pub fn new(title: &str) -> Page {
        let mut page = Page {
            els: Vec::new(),
            sync: BTreeMap::new(),
        };
        page.add(El::new("root", "column").set("gap", crate::value::text("medium")));
        page.add(text("title", "root", "h1", title));
        if let Some(error) = ERROR.lock().unwrap().clone() {
            page.add(
                El::new("error", "notice")
                    .parent("root")
                    .variant("failure")
                    .text(&error),
            );
        }
        page
    }

    pub fn add(&mut self, el: El) {
        self.els.push(el);
    }

    /// Set the signal `sync.<key>`, which elements bind to as `sync.<key>`.
    pub fn sync(&mut self, key: &str, value: Cbor) {
        self.sync.insert(key.to_owned(), value);
    }
}

static ERROR: Mutex<Option<String>> = Mutex::new(None);

/// Show `message` under the title until the next action.
pub fn fail(message: impl Into<String>) {
    *ERROR.lock().unwrap() = Some(message.into());
}

pub fn clear_error() {
    *ERROR.lock().unwrap() = None;
}

/// What a destructive button asked about on the last action, and on the one before it.
static CONFIRM: Mutex<(String, String)> = Mutex::new((String::new(), String::new()));

/// A new action begins: what was asked last time can now be confirmed, and nothing older can.
pub fn begin_action() {
    let mut confirm = CONFIRM.lock().unwrap();
    confirm.1 = std::mem::take(&mut confirm.0);
}

/// Whether the user already pressed this destructive button once, just before. If not, the app
/// shows "Press again" on it and this is false.
pub fn confirmed(key: &str) -> bool {
    let mut confirm = CONFIRM.lock().unwrap();
    if confirm.1 == key {
        confirm.1.clear();
        return true;
    }
    confirm.0 = key.to_owned();
    false
}

/// Whether the button for `key` is waiting for its second press.
pub fn asking(key: &str) -> bool {
    CONFIRM.lock().unwrap().0 == key
}

// Shorthands for the elements apps use most.

pub fn row(id: &str, parent: &str) -> El {
    El::new(id, "row").parent(parent)
}

pub fn column(id: &str, parent: &str) -> El {
    El::new(id, "column").parent(parent)
}

pub fn card(id: &str, parent: &str, title: &str) -> El {
    El::new(id, "card").parent(parent).title(title)
}

/// `variant`: h1, h2, h3, body, caption or code.
pub fn text(id: &str, parent: &str, variant: &str, value: &str) -> El {
    El::new(id, "text")
        .parent(parent)
        .variant(variant)
        .text(value)
}

/// `variant`: neutral, info, success, warning or failure.
pub fn badge(id: &str, parent: &str, variant: &str, value: &str) -> El {
    El::new(id, "badge")
        .parent(parent)
        .variant(variant)
        .text(value)
}

/// `variant`: primary, secondary, plain, success or failure.
pub fn button(
    id: &str,
    parent: &str,
    variant: &str,
    label: &str,
    action: &str,
    payload: Cbor,
) -> El {
    El::new(id, "button")
        .parent(parent)
        .variant(variant)
        .label(label)
        .on_press(action, payload)
}

/// A text field bound to the signal `bind`.
pub fn input(id: &str, parent: &str, label: &str, bind: &str) -> El {
    El::new(id, "input").parent(parent).label(label).bind(bind)
}

#[derive(Default)]
struct Ui {
    last: Vec<Node>,
    sync: BTreeMap<String, Cbor>,
}

static UI: OnceLock<Mutex<Ui>> = OnceLock::new();

fn ui() -> &'static Mutex<Ui> {
    UI.get_or_init(|| Mutex::new(Ui::default()))
}

/// A new session: the renderer starts with an empty tree.
pub fn reset() {
    *ui().lock().unwrap() = Ui::default();
    clear_error();
}

/// Start the session: metadata, initial signals (`form`, and the page's `sync`), then the tree.
pub fn start(host: &Host, name: &str, form: Cbor, page: Page) {
    let roots = tree(page.els);
    let signals = crate::value::map([
        ("form", form),
        ("sync", Cbor::Map(sync_entries(&page.sync))),
    ]);
    let mut out = String::new();
    out.push_str(&ServerMessage::Metadata(Metadata::new().name(name)).to_line());
    out.push_str(&ServerMessage::signal(ciborium_to_json(&signals)).to_line());
    for message in vtree::render_into(None, &roots) {
        out.push_str(&message.to_line());
    }
    host.emit_text(&out);
    let mut ui = ui().lock().unwrap();
    ui.last = roots;
    ui.sync = page.sync;
}

/// Re-send what changed: elements, and signals the app owns.
pub fn update(host: &Host, page: Page) {
    send(host, page, false);
}

/// Like [`update`], but resend every `sync` signal: after an action, the user's controls may be
/// ahead of the document (a checkbox ticked, an action refused).
pub fn refresh(host: &Host, page: Page) {
    send(host, page, true);
}

fn send(host: &Host, page: Page, force: bool) {
    let roots = tree(page.els);
    let mut ui = ui().lock().unwrap();
    let mut out = String::new();

    // A merge patch: what is new or different, and null for what is gone.
    let mut patch: Vec<(Cbor, Cbor)> = Vec::new();
    for (key, value) in &page.sync {
        if force || ui.sync.get(key) != Some(value) {
            patch.push((crate::value::text(key), value.clone()));
        }
    }
    for key in ui.sync.keys().filter(|k| !page.sync.contains_key(*k)) {
        patch.push((crate::value::text(key), Cbor::Null));
    }
    if !patch.is_empty() {
        let patch = crate::value::map([("sync", Cbor::Map(patch))]);
        out.push_str(&ServerMessage::signal(ciborium_to_json(&patch)).to_line());
    }
    for message in vtree::diff_into(None, &ui.last, &roots) {
        out.push_str(&message.to_line());
    }
    host.emit_text(&out);
    ui.last = roots;
    ui.sync = page.sync;
}

fn sync_entries(sync: &BTreeMap<String, Cbor>) -> Vec<(Cbor, Cbor)> {
    sync.iter()
        .map(|(k, v)| (crate::value::text(k), v.clone()))
        .collect()
}

/// Send one signal patch (a JSON Merge Patch), for example to clear an input after an action.
pub fn signal(host: &Host, patch: Cbor) {
    host.emit_text(&ServerMessage::signal(ciborium_to_json(&patch)).to_line());
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
