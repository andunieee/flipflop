//! Apps: WebAssembly modules shared in a group and run on every member's device. Each app owns a
//! section of the group document, `apps/<app id>`, where it keeps whatever it likes; it reads and
//! writes it by path, can read the rest of the document, and is told what changed, as patches.
//! Everything that crosses between an app and the host is CBOR.
//!
//! # The module
//!
//! A core wasm module, in any language, with no WASI. It
//! - exports `memory`;
//! - exports `ff_alloc(len: i32) -> i32`, room for `len` bytes, where the host writes an event;
//! - exports `ff_event(ptr: i32, len: i32)`, which handles one event (the buffer is then the app's to
//!   free);
//! - imports `flipflop.call(ptr: i32, len: i32) -> i32` to make a request: it returns the length of
//!   the response, which the app fetches with
//! - `flipflop.take(ptr: i32)`, copying it to `ptr`, where the app made room for it.
//!
//! The app runs only while it handles an event, with [`FUEL`] to spend and [`MEMORY`] at most.
//! An app that traps or runs out is stopped.
//!
//! # Events (host to app)
//!
//! - `{"type": "start", "app": <id>, "me": <our peer id>}`: first, once.
//! - `{"type": "change", "patches": [<patch>…]}`: the document changed in the app's section or in
//!   a part it watches, whoever changed it (its own writes come back too: the document is the truth).
//!
//! A patch is `{"root": "app" | "group", "path": […], "action": …}`, with `path` relative to the
//! app's section (root `app`) or the document's root (root `group`), and the action one of
//! - `"put"`, `"value"`: the value at `path` is now `value` (an object comes empty, filled by the
//!   patches after it);
//! - `"insert"`, `"values"`: `values` were inserted into a list, the first at the index `path` ends with;
//! - `"delete"` (and `"count"` in a list): the key, or `count` items from the index, are gone;
//! - `"increment"`, `"by"`: the counter at `path` went up by `by`;
//! - `"splice"`, `"index"`, `"text"`: `text` was inserted into the text at `path`, at `index`
//!   (deletions from a text are `delete`s, `path` ending with the index).
//!
//! # Requests (app to host)
//!
//! A map with an `"op"`; the response is `{"ok": <value>}` or `{"err": <message>}`. A `path` is an
//! array of map keys (strings) and list indices (unsigned integers), below the app's section, or
//! below the document's root with `"root": "group"`, which only reads may use.
//!
//! | op          | fields                         | does                                               |
//! |-------------|--------------------------------|----------------------------------------------------|
//! | `get`       | `path`, `root`?                | the value there (objects with all they hold), null if none |
//! | `keys`      | `path`, `root`?                | the keys of the map there                          |
//! | `len`       | `path`, `root`?                | the length of the list, text or map there          |
//! | `put`       | `path`, `value`                | set a map key or list item (objects are made whole) |
//! | `insert`    | `path`, `value`                | insert into a list; `path` ends with the index     |
//! | `delete`    | `path`                         | remove a map key or list item                      |
//! | `splice`    | `path`, `pos`, `del`?, `text`? | edit the text at `path` (positions in characters)  |
//! | `increment` | `path`, `by`                   | add to the counter there                           |
//! | `watch`     | `path`                         | also get the patches under this path from the root |
//! | `unwatch`   | `path`                         | stop that                                          |
//! | `log`       | `msg`                          | write to the app's log                             |
//!
//! # Values
//!
//! Plain CBOR: maps (string keys) and arrays are automerge maps and lists, then strings, integers,
//! floats, booleans, null and byte strings. Three tags carry the rest: [`codec::TAG_TEXT`] around a
//! string is a text object (one people edit together, with `splice`), [`codec::TAG_COUNTER`]
//! around an integer a counter, [`codec::TAG_TIMESTAMP`] around milliseconds a timestamp.

mod codec;

use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use automerge::{ChangeHash, Prop, ROOT, ReadDoc, transaction::Transactable};
use ciborium::Value as Cbor;
use wasmi::{
    Caller, Config, Engine, Extern, Linker, Memory, Module, Store, StoreLimits, StoreLimitsBuilder,
    TypedFunc,
};

use crate::groupdoc::GroupDoc;
use codec::{Change, map, text};

/// What an app may spend on one event (about as many wasm instructions).
pub const FUEL: u64 = 500_000_000;
/// The most memory an app may have.
pub const MEMORY: usize = 64 << 20;

/// Where an app's document lives.
pub trait DocHost: Send + Sync {
    /// Run `f` on the group's document. The app runs in [`App::start`] and [`App::notify`] and calls
    /// this from there, so their callers must not hold the document.
    fn with_doc(&self, f: &mut dyn FnMut(&mut GroupDoc));
}

fn with_doc<R>(host: &dyn DocHost, f: impl FnOnce(&mut GroupDoc) -> R) -> R {
    let (mut f, mut out) = (Some(f), None);
    host.with_doc(&mut |d| out = f.take().map(|f| f(d)));
    out.expect("with_doc runs its closure")
}

/// Compiles and starts apps. One is enough for all of them.
pub struct Runtime {
    engine: Engine,
}

impl Default for Runtime {
    fn default() -> Self {
        let mut config = Config::default();
        config.consume_fuel(true);
        Runtime {
            engine: Engine::new(&config),
        }
    }
}

impl Runtime {
    /// Instantiate the module `wasm` as the app `app`, run by us (`me`) in `host`'s document.
    /// Nothing of the app runs yet (but a wasm start function); [`App::start`] does that.
    pub fn load(&self, wasm: &[u8], app: &str, me: &str, host: Arc<dyn DocHost>) -> Result<App> {
        let module = Module::new(&self.engine, wasm).context("not a wasm module")?;
        let state = State {
            app: app.to_owned(),
            me: me.to_owned(),
            host,
            limits: StoreLimitsBuilder::new().memory_size(MEMORY).build(),
            response: Vec::new(),
            watches: Vec::new(),
            wrote: false,
        };
        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limits);
        store.set_fuel(FUEL)?;
        let mut linker = Linker::<State>::new(&self.engine);
        linker.func_wrap("flipflop", "call", host_call)?;
        linker.func_wrap("flipflop", "take", host_take)?;
        let instance = linker.instantiate_and_start(&mut store, &module)?;
        Ok(App {
            memory: instance
                .get_memory(&store, "memory")
                .ok_or_else(|| anyhow!("the app exports no memory"))?,
            alloc: instance.get_typed_func(&store, "ff_alloc")?,
            event: instance.get_typed_func(&store, "ff_event")?,
            store,
            seen: Vec::new(),
            stopped: false,
        })
    }
}

/// A running app.
pub struct App {
    store: Store<State>,
    memory: Memory,
    alloc: TypedFunc<i32, i32>,
    event: TypedFunc<(i32, i32), ()>,
    /// The heads the app was told about.
    seen: Vec<ChangeHash>,
    /// It failed; it gets no more events.
    stopped: bool,
}

impl App {
    /// Make the app's section if needed and send the start event. Returns whether the document
    /// changed (so it needs saving and syncing).
    pub fn start(&mut self) -> Result<bool> {
        let state = self.store.data();
        let (host, app) = (state.host.clone(), state.app.clone());
        let (made, heads) = with_doc(&*host, |d| {
            let before = d.heads();
            d.app_section(&app);
            let heads = d.heads();
            (heads != before, heads)
        });
        self.seen = heads;
        let event = map([
            ("type", text("start")),
            ("app", text(app)),
            ("me", text(self.store.data().me.as_str())),
        ]);
        Ok(self.send(&event)? || made)
    }

    /// The document changed: tell the app what changed of what it sees, if anything. Returns
    /// whether the app changed the document in turn.
    pub fn notify(&mut self) -> Result<bool> {
        if self.stopped {
            bail!("the app was stopped");
        }
        let state = self.store.data();
        let host = state.host.clone();
        let seen = &self.seen;
        let (heads, patches) = with_doc(&*host, |d| {
            let heads = d.heads();
            let patches = if &heads == seen {
                Vec::new()
            } else {
                d.diff(seen, &heads)
            };
            (heads, patches)
        });
        self.seen = heads;
        let section = [Prop::Map("apps".into()), Prop::Map(state.app.clone())];
        let patches: Vec<Cbor> = patches
            .iter()
            .filter_map(Change::from_patch)
            .filter_map(|c| {
                if c.path.starts_with(&section) {
                    Some(c.encode("app", section.len()))
                } else if state
                    .watches
                    .iter()
                    .any(|w| c.path.starts_with(w) || w.starts_with(&c.path))
                {
                    Some(c.encode("group", 0))
                } else {
                    None
                }
            })
            .collect();
        if patches.is_empty() {
            return Ok(false);
        }
        self.send(&map([
            ("type", text("change")),
            ("patches", Cbor::Array(patches)),
        ]))
    }

    /// Hand the app one event. Returns whether it wrote to the document.
    fn send(&mut self, event: &Cbor) -> Result<bool> {
        if self.stopped {
            bail!("the app was stopped");
        }
        let result = self.deliver(&codec::encode(event));
        self.stopped = result.is_err();
        result?;
        Ok(std::mem::take(&mut self.store.data_mut().wrote))
    }

    fn deliver(&mut self, bytes: &[u8]) -> Result<()> {
        let len = i32::try_from(bytes.len())?;
        self.store.set_fuel(FUEL)?;
        let ptr = self.alloc.call(&mut self.store, len)?;
        self.memory
            .write(&mut self.store, ptr as u32 as usize, bytes)
            .context("ff_alloc gave room outside memory")?;
        self.event.call(&mut self.store, (ptr, len))?;
        Ok(())
    }
}

struct State {
    app: String,
    me: String,
    host: Arc<dyn DocHost>,
    limits: StoreLimits,
    /// The response to the last request, until the app takes it.
    response: Vec<u8>,
    /// Paths from the document's root the app also wants patches for.
    watches: Vec<Vec<Prop>>,
    /// The app wrote to the document since it was last asked.
    wrote: bool,
}

fn memory(caller: &Caller<'_, State>) -> Result<Memory, wasmi::Error> {
    caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .ok_or_else(|| wasmi::Error::new("the app exports no memory"))
}

fn host_call(mut caller: Caller<'_, State>, ptr: i32, len: i32) -> Result<i32, wasmi::Error> {
    let memory = memory(&caller)?;
    let request = memory
        .data(&caller)
        .get(ptr as u32 as usize..)
        .and_then(|m| m.get(..len as u32 as usize))
        .ok_or_else(|| wasmi::Error::new("call: request outside memory"))?;
    let response = match ciborium::from_reader::<Cbor, _>(request) {
        Ok(request) => caller.data_mut().request(request),
        Err(e) => Err(anyhow!("request is not CBOR: {e}")),
    };
    let response = match response {
        Ok(v) => map([("ok", v)]),
        Err(e) => map([("err", text(format!("{e:#}")))]),
    };
    let state = caller.data_mut();
    state.response = codec::encode(&response);
    Ok(state.response.len() as i32)
}

fn host_take(mut caller: Caller<'_, State>, ptr: i32) -> Result<(), wasmi::Error> {
    let memory = memory(&caller)?;
    let response = std::mem::take(&mut caller.data_mut().response);
    memory
        .write(&mut caller, ptr as u32 as usize, &response)
        .map_err(|_| wasmi::Error::new("take: room outside memory"))
}

/// A request's fields.
struct Request(Vec<(Cbor, Cbor)>);

impl Request {
    fn get(&self, name: &str) -> Option<&Cbor> {
        self.0
            .iter()
            .find(|(k, _)| k.as_text() == Some(name))
            .map(|(_, v)| v)
    }

    fn take(&mut self, name: &str) -> Result<Cbor> {
        let i = self
            .0
            .iter()
            .position(|(k, _)| k.as_text() == Some(name))
            .ok_or_else(|| anyhow!("missing {name}"))?;
        Ok(self.0.swap_remove(i).1)
    }

    fn str(&self, name: &str) -> Result<&str> {
        self.get(name)
            .and_then(Cbor::as_text)
            .ok_or_else(|| anyhow!("{name} must be a string"))
    }

    fn uint(&self, name: &str, default: Option<usize>) -> Result<usize> {
        match self.get(name) {
            None => default.ok_or_else(|| anyhow!("missing {name}")),
            Some(Cbor::Integer(i)) => Ok(usize::try_from(i128::from(*i))?),
            Some(_) => bail!("{name} must be an unsigned integer"),
        }
    }

    fn path(&self) -> Result<Vec<Prop>> {
        self.get("path")
            .map_or(Ok(Vec::new()), codec::path)
            .context("bad path")
    }
}

impl State {
    fn request(&mut self, request: Cbor) -> Result<Cbor> {
        let Cbor::Map(fields) = request else {
            bail!("a request is a map")
        };
        let mut req = Request(fields);
        let op = req.str("op")?.to_owned();
        let path = req.path()?;
        let group = match req.get("root").map(Cbor::as_text) {
            None | Some(Some("app")) => false,
            Some(Some("group")) => true,
            _ => bail!("root is \"app\" or \"group\""),
        };
        let writes = matches!(
            op.as_str(),
            "put" | "insert" | "delete" | "splice" | "increment"
        );
        if group && writes {
            bail!("only the app's own section can be written");
        }
        // Before trying: a write that fails halfway may have written something.
        self.wrote |= writes;
        let app = self.app.clone();
        with_doc(&*self.host, |d| {
            let base = match group {
                true => ROOT,
                false => d
                    .app_section(&app)
                    .ok_or_else(|| anyhow!("the app's section is gone"))?,
            };
            let doc = d.raw();
            Ok(match op.as_str() {
                "get" if path.is_empty() => codec::read_obj(doc, &base),
                "get" => match codec::split(doc, &base, &path) {
                    Ok((parent, prop)) => codec::read_prop(doc, &parent, prop),
                    Err(_) => Cbor::Null,
                },
                "keys" => {
                    let obj = codec::resolve(doc, &base, &path)?;
                    Cbor::Array(doc.keys(&obj).map(Cbor::Text).collect())
                }
                "len" => {
                    let obj = codec::resolve(doc, &base, &path)?;
                    Cbor::Integer((doc.length(&obj) as u64).into())
                }
                "put" => {
                    let (parent, prop) = codec::split(doc, &base, &path)?;
                    codec::put(doc, &parent, prop, req.take("value")?)?;
                    Cbor::Null
                }
                "insert" => {
                    let (parent, Prop::Seq(index)) = codec::split(doc, &base, &path)? else {
                        bail!("insert's path ends with a list index");
                    };
                    codec::insert(doc, &parent, index, req.take("value")?)?;
                    Cbor::Null
                }
                "delete" => {
                    let (parent, prop) = codec::split(doc, &base, &path)?;
                    doc.delete(&parent, prop)?;
                    Cbor::Null
                }
                "splice" => {
                    let obj = codec::resolve(doc, &base, &path)?;
                    let (pos, del) = (req.uint("pos", None)?, req.uint("del", Some(0))?);
                    let text = req.get("text").and_then(Cbor::as_text).unwrap_or("");
                    doc.splice_text(&obj, pos, del as isize, text)?;
                    Cbor::Null
                }
                "increment" => {
                    let (parent, prop) = codec::split(doc, &base, &path)?;
                    let by = match req.get("by") {
                        Some(Cbor::Integer(i)) => i64::try_from(i128::from(*i))?,
                        _ => bail!("by must be an integer"),
                    };
                    doc.increment(&parent, prop, by)?;
                    Cbor::Null
                }
                "watch" | "unwatch" | "log" => Cbor::Null,
                _ => bail!("unknown op {op}"),
            })
        })
        .inspect(|_| match op.as_str() {
            "watch" => self.watches.push(path),
            "unwatch" => self.watches.retain(|w| *w != path),
            "log" => tracing::info!(app = %self.app, "{}", req.str("msg").unwrap_or("")),
            _ => {}
        })
    }
}

#[cfg(test)]
mod tests;
