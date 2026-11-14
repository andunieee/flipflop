//! A real hypereffect renderer core (`hypereffect::client::Client`) for the miniapp examples: it
//! keeps the element tree, evaluates CEL, applies signals and yields the resolved tree. Printing and
//! comparing it replaces the old hand-written frame application.

use anyhow::{Context, Result, bail};
use ciborium::Value as Cbor;
use hypereffect::client::Client;
use hypereffect::message::ServerMessage;
use serde_json::Value as Json;

pub fn from_json(v: Json) -> Cbor {
    match v {
        Json::Null => Cbor::Null,
        Json::Bool(b) => Cbor::Bool(b),
        Json::Number(n) => {
            if let Some(i) = n.as_i64() {
                Cbor::Integer(i.into())
            } else if let Some(u) = n.as_u64() {
                Cbor::Integer(u.into())
            } else {
                Cbor::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        Json::String(s) => Cbor::Text(s),
        Json::Array(a) => Cbor::Array(a.into_iter().map(from_json).collect()),
        Json::Object(o) => Cbor::Map(
            o.into_iter()
                .map(|(k, v)| (Cbor::Text(k), from_json(v)))
                .collect(),
        ),
    }
}

pub struct Renderer {
    pub client: Client,
}

impl Renderer {
    pub fn new() -> Self {
        let mut client = Client::new();
        // The host starts the wasm app with `start`; the renderer only needs to be connected so
        // `trigger` sends actions.
        let _ = client.hello();
        Renderer { client }
    }

    /// Feed a batch of protocol lines, printing them.
    pub fn apply(&mut self, bytes: &[u8]) -> Result<()> {
        for line in bytes.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
            let line = std::str::from_utf8(line).unwrap_or("<invalid utf-8>");
            println!("{line}");
        }
        self.client.receive(bytes);
        for diagnostic in self.client.take_diagnostics() {
            eprintln!("hypereffect: {diagnostic}");
        }
        Ok(())
    }

    pub fn signals(&self) -> &Json {
        self.client.signals()
    }

    /// Simulate the user editing bound signals before an action.
    pub fn set(&mut self, patch: &Json) {
        if let Json::Object(_) = patch {
            let line = ServerMessage::signal(patch.clone()).to_line();
            self.client.receive(line.as_bytes());
        }
    }

    pub fn print(&self, label: &str) -> Result<()> {
        println!("=== {label} ===");
        println!(
            "signals {}",
            serde_json::to_string_pretty(self.client.signals())?
        );
        println!(
            "tree {}",
            serde_json::to_string_pretty(&self.client.tree())?
        );
        Ok(())
    }

    pub fn same_as(&self, other: &Renderer) -> bool {
        self.client.tree() == other.client.tree() && self.client.signals() == other.client.signals()
    }
}

pub fn read_json(path: &str) -> Result<Json> {
    let text = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}

pub struct Action {
    pub name: String,
    pub payload: Cbor,
    pub set: Option<Json>,
}

/// Read an actions file: a JSON array of `{"name", "payload"?, "set"?}`.
pub fn read_actions(path: &str) -> Result<Vec<Action>> {
    let Json::Array(items) = read_json(path)? else {
        bail!("an actions file is a JSON array")
    };
    let mut out = Vec::new();
    for item in items {
        let Json::Object(mut fields) = item else {
            bail!("each action is a JSON object")
        };
        let name = fields
            .remove("name")
            .and_then(|v| v.as_str().map(str::to_owned))
            .context("an action needs a name")?;
        out.push(Action {
            name,
            payload: fields
                .remove("payload")
                .map(from_json)
                .unwrap_or(Cbor::Null),
            set: fields.remove("set"),
        });
    }
    Ok(out)
}
