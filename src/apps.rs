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
//! - exports `ff_get_params() -> i64`, which describes the parameters the user gives when adding the
//!   app (see "Parameters"); optional, an app without it has none;
//! - exports `ff_start(ptr: i32, len: i32)`, `ff_doc_change(ptr: i32, len: i32)` and
//!   `ff_action(ptr: i32, len: i32)`, which handle one event each (see "Events"; the buffer is then
//!   the app's to free);
//! - imports `flipflop.call(ptr: i32, len: i32) -> i32` to make a request: it returns the length of
//!   the response, which the app fetches with
//! - `flipflop.take(ptr: i32)`, copying it to `ptr`, where the app made room for it;
//! - imports `flipflop.emit(ptr: i32, len: i32)` to send hypereffect lines to the local renderer:
//!   UTF-8 `signal …\n`, `element …\n`, `remove …\n`, `metadata …\n` messages, exactly as the
//!   protocol frames them. The host collects them with [`App::take_ui`]. They are per session and
//!   never go in the document.
//!
//! The app runs only while it handles an event, with [`FUEL`] to spend and [`MEMORY`] at most.
//! An app that traps or runs out is stopped.
//!
//! # Parameters
//!
//! Before an app is added to a group, the host instantiates it alone (no document, nothing started)
//! and calls `ff_get_params()`. It returns an `i64`: the address of a CBOR value in the app's memory
//! in the high 32 bits, its length in the low 32. The value is
//! `{"name": {"default": <str>, "placeholder": <str>}, "params": [{"key": <str>, "label": <str>,
//! "default": <str>, "placeholder": <str>}…]}`. The user sees a form with the name of the app's tab
//! and one text field per param, and the answers (a field left empty takes its default) are kept in
//! the group, in the app's tab. Every device passes them in the `start` event.
//!
//! # Events (host to app)
//!
//! - `ff_start`, `{"app": <id>, "me": <our peer id>, "params": {<key>: <str>…}, "ts": <ms>,
//!   "tz": <minutes>}`: first, once.
//! - `ff_doc_change`, `{"patches": [<patch>…], "ts": <ms>, "tz": <minutes>}`: the document
//!   changed in the app's section or in a part it watches, whoever changed it (its own writes come
//!   back too: the document is the truth).
//! - `ff_action`, `{"name": <str>, "payload": <value>, "signals": <map>, "me": <peer id>,
//!   "ts": <ms>, "tz": <minutes>, "seq": <n>}`: the local hypereffect renderer sent an action. The
//!   app validates it, writes the document, and lets the following `ff_doc_change` update the
//!   interface. `ts` and `seq` let it name new things without a clock or randomness of its own.
//!
//! Every event carries the time: `ts` is the device's clock in epoch milliseconds and `tz` its
//! offset from UTC in minutes (east positive), so an app can show dates the way its user reads them.
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
use automerge::{ChangeHash, ObjType, Prop, ROOT, ReadDoc, Value, transaction::Transactable};
use ciborium::Value as Cbor;
use wasmi::{
    Caller, Config, Engine, Extern, Instance, Linker, Memory, Module, Store, StoreLimits,
    StoreLimitsBuilder, TypedFunc,
};

use crate::groupdoc::GroupDoc;
use codec::{Change, map, text};

/// What an app may spend on one event (about as many wasm instructions).
pub const FUEL: u64 = 500_000_000;
/// The most memory an app may have.
pub const MEMORY: usize = 64 << 20;
/// The most hypereffect output an app may leave untaken by the host; past it the app is stopped.
pub const UI_BACKLOG: usize = 16 << 20;

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

/// Queue a command for `app`, in the queue of the peer whose UI wrote it. The app applies it, and
/// takes it out of the queue, the next time the host notifies it.
pub fn enqueue(doc: &mut GroupDoc, app: &str, by: &str, command: Cbor) -> Result<()> {
    let section = doc
        .app_section(app)
        .ok_or_else(|| anyhow!("the app's section is gone"))?;
    // One flat key per peer's queue: a list its UI made, and only its UI appends to. Every app
    // deletes what it applied. Making a map here would race another device's map and lose a queue.
    let queue_key = format!("inbox\u{1f}{by}");
    let raw = doc.raw();
    let queue = match raw.get(&section, queue_key.as_str())? {
        Some((Value::Object(_), id)) => id,
        _ => raw.put_object(&section, queue_key.as_str(), ObjType::List)?,
    };
    let next = raw.length(&queue);
    codec::insert(raw, &queue, next, command)?;
    Ok(())
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
    /// Ask the module `wasm` what it takes to be added (see "Parameters"). An app without
    /// `ff_get_params` takes nothing.
    pub fn params(&self, wasm: &[u8]) -> Result<AppParams> {
        let (mut store, instance) = self.instantiate(wasm, "", "", Arc::new(NoDoc))?;
        let Ok(get) = instance.get_typed_func::<(), i64>(&store, "ff_get_params") else {
            return Ok(AppParams::default());
        };
        let memory = instance
            .get_memory(&store, "memory")
            .ok_or_else(|| anyhow!("the app exports no memory"))?;
        let packed = get.call(&mut store, ())? as u64;
        let (ptr, len) = ((packed >> 32) as usize, (packed & 0xffff_ffff) as usize);
        let bytes = memory
            .data(&store)
            .get(ptr..)
            .and_then(|m| m.get(..len))
            .ok_or_else(|| anyhow!("ff_get_params returned room outside memory"))?;
        let value: Cbor = ciborium::from_reader(bytes).context("ff_get_params is not CBOR")?;
        let field = |v: &Cbor, k: &str| match v {
            Cbor::Map(m) => m
                .iter()
                .find(|(key, _)| key.as_text() == Some(k))
                .map(|(_, v)| v.clone()),
            _ => None,
        };
        let param = |v: &Cbor| {
            let s = |k: &str| {
                field(v, k)
                    .and_then(|v| v.into_text().ok())
                    .unwrap_or_default()
            };
            Param {
                key: s("key"),
                label: s("label"),
                default: s("default"),
                placeholder: s("placeholder"),
            }
        };
        Ok(AppParams {
            name: field(&value, "name").map(|v| param(&v)).unwrap_or_default(),
            params: match field(&value, "params") {
                Some(Cbor::Array(items)) => items.iter().map(param).collect(),
                _ => Vec::new(),
            },
        })
    }

    /// Instantiate the module `wasm` as the app `app`, run by us (`me`) in `host`'s document.
    /// Nothing of the app runs yet (but a wasm start function); [`App::start`] does that.
    pub fn load(&self, wasm: &[u8], app: &str, me: &str, host: Arc<dyn DocHost>) -> Result<App> {
        let (store, instance) = self.instantiate(wasm, app, me, host)?;
        Ok(App {
            memory: instance
                .get_memory(&store, "memory")
                .ok_or_else(|| anyhow!("the app exports no memory"))?,
            alloc: instance.get_typed_func(&store, "ff_alloc")?,
            on_start: instance.get_typed_func(&store, "ff_start")?,
            on_doc_change: instance.get_typed_func(&store, "ff_doc_change")?,
            on_action: instance.get_typed_func(&store, "ff_action")?,
            store,
            seen: Vec::new(),
            seq: 0,
            stopped: false,
        })
    }

    fn instantiate(
        &self,
        wasm: &[u8],
        app: &str,
        me: &str,
        host: Arc<dyn DocHost>,
    ) -> Result<(Store<State>, Instance)> {
        let module = Module::new(&self.engine, wasm).context("not a wasm module")?;
        let state = State {
            app: app.to_owned(),
            me: me.to_owned(),
            host,
            limits: StoreLimitsBuilder::new().memory_size(MEMORY).build(),
            response: Vec::new(),
            watches: Vec::new(),
            wrote: false,
            out: Vec::new(),
        };
        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limits);
        store.set_fuel(FUEL)?;
        let mut linker = Linker::<State>::new(&self.engine);
        linker.func_wrap("flipflop", "call", host_call)?;
        linker.func_wrap("flipflop", "take", host_take)?;
        linker.func_wrap("flipflop", "emit", host_emit)?;
        let instance = linker.instantiate_and_start(&mut store, &module)?;
        Ok((store, instance))
    }
}

/// What the user may set when adding an app, as the app describes it with `ff_get_params`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AppParams {
    /// The name of the app's tab.
    pub name: Param,
    pub params: Vec<Param>,
}

/// One field of the form shown when adding an app.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Param {
    /// The param's name in the `start` event (unused for the tab's name).
    pub key: String,
    /// What the form calls it.
    pub label: String,
    /// What it is when the user leaves it empty.
    pub default: String,
    /// What the empty field shows.
    pub placeholder: String,
}

impl AppParams {
    /// The tab's name and the params' values, from what the user typed (in the order of the
    /// params): an empty answer is the default, and the tab's name falls back to the placeholder,
    /// then to `fallback`.
    pub fn answers(&self, name: &str, values: &[String]) -> (String, Vec<(String, String)>) {
        let pick = |typed: &str, p: &Param| match typed.trim() {
            "" => p.default.clone(),
            typed => typed.to_owned(),
        };
        let mut name = pick(name, &self.name);
        if name.is_empty() {
            name = self.name.placeholder.clone();
        }
        let params = self
            .params
            .iter()
            .enumerate()
            .map(|(i, p)| {
                (
                    p.key.clone(),
                    pick(values.get(i).map_or("", String::as_str), p),
                )
            })
            .collect();
        (name, params)
    }
}

/// A host for an app that is only asked what it takes: it has no group.
struct NoDoc;

impl DocHost for NoDoc {
    fn with_doc(&self, f: &mut dyn FnMut(&mut GroupDoc)) {
        f(&mut GroupDoc::new());
    }
}

/// Which of the app's handlers an event goes to.
#[derive(Clone, Copy)]
enum Kind {
    Start,
    DocChange,
    Action,
}

/// A running app.
pub struct App {
    store: Store<State>,
    memory: Memory,
    alloc: TypedFunc<i32, i32>,
    on_start: TypedFunc<(i32, i32), ()>,
    on_doc_change: TypedFunc<(i32, i32), ()>,
    on_action: TypedFunc<(i32, i32), ()>,
    /// The heads the app was told about.
    seen: Vec<ChangeHash>,
    /// The number of actions given to the app, for event ids.
    seq: u64,
    /// It failed; it gets no more events.
    stopped: bool,
}

impl App {
    /// Make the app's section if needed and send the start event. Returns whether the document
    /// changed (so it needs saving and syncing). The app's initial hypereffect messages are waiting
    /// in [`App::take_ui`].
    pub fn start(&mut self) -> Result<bool> {
        let state = self.store.data();
        let (host, app) = (state.host.clone(), state.app.clone());
        let (made, heads, params) = with_doc(&*host, |d| {
            let before = d.heads();
            d.app_section(&app);
            let heads = d.heads();
            (heads != before, heads, d.app_params(&app))
        });
        self.seen = heads;
        let params = Cbor::Map(
            params
                .into_iter()
                .map(|(k, v)| (text(k), text(v)))
                .collect(),
        );
        let event = map([
            ("app", text(app)),
            ("params", params),
            ("me", text(self.store.data().me.as_str())),
            ("ts", Cbor::Integer(crate::groupdoc::now_ms().into())),
            ("tz", Cbor::Integer(utc_offset_minutes().into())),
        ]);
        Ok(self.send(Kind::Start, &event)? || made)
    }

    /// The document changed: tell the app what changed of what it sees, if anything. Returns
    /// whether the app changed the document in turn. Any hypereffect messages it emitted are in
    /// [`App::take_ui`].
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
        self.send(
            Kind::DocChange,
            &map([
                ("patches", Cbor::Array(patches)),
                ("ts", Cbor::Integer(crate::groupdoc::now_ms().into())),
                ("tz", Cbor::Integer(utc_offset_minutes().into())),
            ]),
        )
    }

    /// Hand the app one event. Returns whether it wrote to the document.
    fn send(&mut self, kind: Kind, event: &Cbor) -> Result<bool> {
        if self.stopped {
            bail!("the app was stopped");
        }
        let result = self.deliver(kind, &codec::encode(event));
        self.stopped = result.is_err();
        result?;
        Ok(std::mem::take(&mut self.store.data_mut().wrote))
    }

    /// Whether the app failed and gets no more events.
    pub fn is_stopped(&self) -> bool {
        self.stopped
    }

    /// The hypereffect messages the app emitted since this was last called, in order, as the UTF-8
    /// lines of the protocol (feed them to `hypereffect::client::Client::receive`).
    pub fn take_ui(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.store.data_mut().out)
    }

    /// Give the app one action from the local hypereffect renderer. `signals` are the non-local
    /// signals at the time of the action. Returns whether the app changed the document; if it did,
    /// call [`App::notify`] to let it (and every other device) render the result. Any messages the
    /// action itself emitted are in [`App::take_ui`].
    pub fn action(&mut self, name: &str, payload: Cbor, signals: Cbor) -> Result<bool> {
        if self.stopped {
            bail!("the app was stopped");
        }
        self.seq += 1;
        let me = self.store.data().me.clone();
        let event = map([
            ("name", text(name)),
            ("payload", payload),
            ("signals", signals),
            ("me", text(me)),
            ("ts", Cbor::Integer(crate::groupdoc::now_ms().into())),
            ("tz", Cbor::Integer(utc_offset_minutes().into())),
            ("seq", Cbor::Integer(self.seq.into())),
        ]);
        self.send(Kind::Action, &event)
    }

    fn deliver(&mut self, kind: Kind, bytes: &[u8]) -> Result<()> {
        let len = i32::try_from(bytes.len())?;
        self.store.set_fuel(FUEL)?;
        let ptr = self.alloc.call(&mut self.store, len)?;
        self.memory
            .write(&mut self.store, ptr as u32 as usize, bytes)
            .context("ff_alloc gave room outside memory")?;
        let handler = match kind {
            Kind::Start => &self.on_start,
            Kind::DocChange => &self.on_doc_change,
            Kind::Action => &self.on_action,
        };
        handler.call(&mut self.store, (ptr, len))?;
        Ok(())
    }
}

/// This device's offset from UTC, in minutes.
#[cfg(not(target_arch = "wasm32"))]
fn utc_offset_minutes() -> i64 {
    use chrono::Offset;
    i64::from(chrono::Local::now().offset().fix().local_minus_utc()) / 60
}

#[cfg(target_arch = "wasm32")]
fn utc_offset_minutes() -> i64 {
    0
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
    /// Hypereffect messages emitted since the host last took them, as protocol lines.
    out: Vec<u8>,
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

/// Hypereffect lines: UTF-8 `signal …\n`, `element …\n`, … sent by the app. They are queued for
/// [`App::take_ui`], not written to the document.
fn host_emit(mut caller: Caller<'_, State>, ptr: i32, len: i32) -> Result<(), wasmi::Error> {
    let memory = memory(&caller)?;
    let bytes = memory
        .data(&caller)
        .get(ptr as u32 as usize..)
        .and_then(|m| m.get(..len as u32 as usize))
        .ok_or_else(|| wasmi::Error::new("emit: messages outside memory"))?
        .to_vec();
    let out = &mut caller.data_mut().out;
    if out.len() + bytes.len() > UI_BACKLOG {
        return Err(wasmi::Error::new(
            "emit: too many messages the renderer has not taken",
        ));
    }
    out.extend_from_slice(&bytes);
    Ok(())
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
            "watch" if !self.watches.contains(&path) => self.watches.push(path),
            "unwatch" => self.watches.retain(|w| *w != path),
            "log" => tracing::info!(app = %self.app, "{}", req.str("msg").unwrap_or("")),
            _ => {}
        })
    }
}

#[cfg(test)]
mod tests;
