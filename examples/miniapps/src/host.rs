//! The host side of the app ABI: one request, one response, as CBOR (see `src/apps.rs`).

use ciborium::Value as Cbor;

use crate::value::{key, path, text};

#[link(wasm_import_module = "flipflop")]
extern "C" {
    fn call(ptr: *const u8, len: usize) -> usize;
    fn take(ptr: *mut u8);
    fn emit(ptr: *const u8, len: usize);
}

pub struct Host;

impl Host {
    /// Make a request; the value of `ok`, or null (with the error logged) if it failed.
    fn request(&self, request: Cbor) -> Cbor {
        let mut bytes = Vec::new();
        if ciborium::into_writer(&request, &mut bytes).is_err() {
            return Cbor::Null;
        }
        let len = unsafe { call(bytes.as_ptr(), bytes.len()) };
        let mut response = vec![0u8; len];
        unsafe { take(response.as_mut_ptr()) };
        let value = ciborium::from_reader::<Cbor, _>(response.as_slice()).unwrap_or(Cbor::Null);
        if let Some(err) = super::value::field(&value, "err").and_then(Cbor::as_text) {
            self.log(&format!("request failed: {err}"));
            return Cbor::Null;
        }
        super::value::field(&value, "ok")
            .cloned()
            .unwrap_or(Cbor::Null)
    }

    fn op(
        &self,
        op: &str,
        p: &[Cbor],
        extra: impl IntoIterator<Item = (&'static str, Cbor)>,
    ) -> Cbor {
        let mut entries = vec![(text("op"), text(op)), (text("path"), path(p))];
        entries.extend(extra.into_iter().map(|(k, v)| (text(k), v)));
        self.request(Cbor::Map(entries))
    }

    pub fn get(&self, p: &[Cbor]) -> Cbor {
        self.op("get", p, [])
    }

    /// Read a path from the document's root, below the app's section.
    pub fn group(&self, p: &[Cbor]) -> Cbor {
        self.op("get", p, [("root", text("group"))])
    }

    pub fn keys(&self, p: &[Cbor]) -> Vec<String> {
        match self.op("keys", p, []) {
            Cbor::Array(items) => items
                .into_iter()
                .filter_map(|v| v.as_text().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        }
    }

    pub fn len(&self, p: &[Cbor]) -> usize {
        match self.op("len", p, []) {
            Cbor::Integer(i) => usize::try_from(i128::from(i)).unwrap_or(0),
            _ => 0,
        }
    }

    pub fn put(&self, p: &[Cbor], value: Cbor) {
        self.op("put", p, [("value", value)]);
    }

    pub fn insert(&self, p: &[Cbor], value: Cbor) {
        self.op("insert", p, [("value", value)]);
    }

    pub fn delete(&self, p: &[Cbor]) {
        self.op("delete", p, []);
    }

    pub fn increment(&self, p: &[Cbor], by: i64) {
        self.op("increment", p, [("by", Cbor::Integer(by.into()))]);
    }

    /// Get patches under a path from the document's root (not just this app's section).
    pub fn watch(&self, p: &[Cbor]) {
        self.op("watch", p, []);
    }

    pub fn log(&self, msg: &str) {
        self.op("log", &[], [("msg", text(msg))]);
    }

    /// Send hypereffect lines to the local renderer. The host takes them with `App::take_ui`.
    pub fn emit_text(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        unsafe { emit(text.as_ptr(), text.len()) };
    }

    /// `members`: peer id -> name, from the group document.
    pub fn members(&self) -> Vec<(String, String)> {
        super::value::text_entries(&self.group(&[key("members")]))
            .into_iter()
            .map(|(id, name)| (id.to_owned(), name.as_text().unwrap_or(id).to_owned()))
            .collect()
    }
}
