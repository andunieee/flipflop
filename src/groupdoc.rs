//! The shared state of one group: an Automerge document holding its members, file offers, text
//! snippets and scratchpad. Pure data, no networking; [`crate::core`] moves the sync messages.
//!
//! Layout (all under the root map):
//! - `members`: peer id -> device name. Every peer only writes its own entry.
//! - `files`:   `<millis>-<rand>` -> { name, size, hash, kind, from, from_name, ts, stopped, downloaded: { peer id -> ts } }
//! - `texts`:   `<millis>-<rand>` -> { from, name, text, ts }
//! - `scratch`: a text object everyone edits.
//!
//! Entry keys start with the creation time, so key order is chronological.

use std::collections::{BTreeMap, HashMap};

use automerge::{
    ActorId, AutoCommit, AutomergeError, ObjId, ObjType, ROOT, ReadDoc, Value, sync,
    sync::SyncDoc,
    transaction::{CommitOptions, Transactable},
};

#[derive(Debug, Clone, PartialEq)]
pub struct FileOffer {
    pub id: String,
    pub name: String,
    pub size: u64,
    /// blake3, hex.
    pub hash: String,
    /// "file", "media" or "apk".
    pub kind: String,
    /// Peer id of the offerer, who serves the bytes.
    pub from: String,
    pub from_name: String,
    pub ts: i64,
    pub stopped: bool,
    /// Peer id -> when they finished downloading.
    pub downloaded: BTreeMap<String, i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextEntry {
    pub id: String,
    pub from: String,
    pub name: String,
    pub text: String,
    pub ts: i64,
}

pub struct GroupDoc {
    doc: AutoCommit,
    sync: HashMap<String, sync::State>,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// `n` random bytes, hex encoded. (Borrows iroh's key generator as a source of randomness.)
pub fn random_hex(n: usize) -> String {
    iroh::SecretKey::generate().to_bytes()[..n].iter().map(|b| format!("{b:02x}")).collect()
}

fn entry_key() -> String {
    format!("{:013}-{}", now_ms(), random_hex(4))
}

impl GroupDoc {
    /// A fresh document. Every peer builds the identical genesis change (fixed actor, no timestamp),
    /// so independently created copies merge instead of fighting over the root objects.
    pub fn new() -> GroupDoc {
        let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![0u8; 16]));
        for (key, obj) in [("members", ObjType::Map), ("files", ObjType::Map), ("texts", ObjType::Map), ("scratch", ObjType::Text)] {
            doc.put_object(ROOT, key, obj).expect("genesis");
        }
        doc.commit_with(CommitOptions::default().with_time(0));
        doc.set_actor(ActorId::random());
        GroupDoc { doc, sync: HashMap::new() }
    }

    pub fn load(bytes: &[u8]) -> Result<GroupDoc, AutomergeError> {
        let mut doc = AutoCommit::load(bytes)?;
        doc.set_actor(ActorId::random());
        Ok(GroupDoc { doc, sync: HashMap::new() })
    }

    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    fn obj(&self, key: &str) -> ObjId {
        match self.doc.get(ROOT, key) {
            Ok(Some((Value::Object(_), id))) => id,
            _ => panic!("group doc is missing {key}"),
        }
    }

    fn str_of(&self, obj: &ObjId, key: &str) -> String {
        match self.doc.get(obj, key) {
            Ok(Some((Value::Scalar(s), _))) => s.as_str().map(str::to_owned).unwrap_or_default(),
            _ => String::new(),
        }
    }

    fn int_of(&self, obj: &ObjId, key: &str) -> i64 {
        match self.doc.get(obj, key) {
            Ok(Some((Value::Scalar(s), _))) => s.as_i64().unwrap_or(0),
            _ => 0,
        }
    }

    fn bool_of(&self, obj: &ObjId, key: &str) -> bool {
        match self.doc.get(obj, key) {
            Ok(Some((Value::Scalar(s), _))) => s.as_bool().unwrap_or(false),
            _ => false,
        }
    }

    fn entries(&self, obj: &ObjId) -> Vec<(String, ObjId)> {
        self.doc
            .map_range(obj, ..)
            .filter(|item| matches!(item.value, automerge::ValueRef::Object(_)))
            .map(|item| (item.key.to_string(), item.id()))
            .collect()
    }

    // ---- members ----------------------------------------------------------------------

    /// Record (or refresh) our own entry.
    pub fn join(&mut self, me: &str, name: &str) {
        let members = self.obj("members");
        if self.str_of(&members, me) != name {
            self.doc.put(&members, me, name).expect("put member");
        }
    }

    /// `(peer id, device name)`, sorted by id.
    pub fn members(&self) -> Vec<(String, String)> {
        let members = self.obj("members");
        self.doc
            .map_range(&members, ..)
            .filter_map(|item| Some((item.key.to_string(), item.value.into_value().as_str()?.to_owned())))
            .collect()
    }

    pub fn is_member(&self, peer: &str) -> bool {
        self.doc.get(self.obj("members"), peer).ok().flatten().is_some()
    }

    // ---- files ------------------------------------------------------------------------

    pub fn offer_file(&mut self, me: &str, my_name: &str, name: &str, size: u64, hash: &str, kind: &str) -> String {
        let files = self.obj("files");
        let id = entry_key();
        let f = self.doc.put_object(&files, id.as_str(), ObjType::Map).expect("put file");
        self.doc.put(&f, "name", name).unwrap();
        self.doc.put(&f, "size", size as i64).unwrap();
        self.doc.put(&f, "hash", hash).unwrap();
        self.doc.put(&f, "kind", kind).unwrap();
        self.doc.put(&f, "from", me).unwrap();
        self.doc.put(&f, "from_name", my_name).unwrap();
        self.doc.put(&f, "ts", now_ms()).unwrap();
        self.doc.put(&f, "stopped", false).unwrap();
        self.doc.put_object(&f, "downloaded", ObjType::Map).unwrap();
        id
    }

    fn file_obj(&self, id: &str) -> Option<ObjId> {
        match self.doc.get(self.obj("files"), id) {
            Ok(Some((Value::Object(_), obj))) => Some(obj),
            _ => None,
        }
    }

    /// Stop offering a file. The entry stays (so concurrent "downloaded" marks still merge).
    pub fn stop_file(&mut self, id: &str) {
        if let Some(f) = self.file_obj(id) {
            let _ = self.doc.put(&f, "stopped", true);
        }
    }

    pub fn mark_downloaded(&mut self, id: &str, me: &str) {
        let Some(f) = self.file_obj(id) else { return };
        if let Ok(Some((Value::Object(_), d))) = self.doc.get(&f, "downloaded") {
            let _ = self.doc.put(&d, me, now_ms());
        }
    }

    pub fn file(&self, id: &str) -> Option<FileOffer> {
        self.file_obj(id).map(|obj| self.read_file(id.to_owned(), &obj))
    }

    fn read_file(&self, id: String, f: &ObjId) -> FileOffer {
        let downloaded = match self.doc.get(f, "downloaded") {
            Ok(Some((Value::Object(_), d))) => self
                .doc
                .map_range(&d, ..)
                .filter_map(|item| Some((item.key.to_string(), item.value.into_value().as_i64()?)))
                .collect(),
            _ => BTreeMap::new(),
        };
        FileOffer {
            id,
            name: self.str_of(f, "name"),
            size: self.int_of(f, "size").max(0) as u64,
            hash: self.str_of(f, "hash"),
            kind: self.str_of(f, "kind"),
            from: self.str_of(f, "from"),
            from_name: self.str_of(f, "from_name"),
            ts: self.int_of(f, "ts"),
            stopped: self.bool_of(f, "stopped"),
            downloaded,
        }
    }

    /// Oldest first.
    pub fn files(&self) -> Vec<FileOffer> {
        self.entries(&self.obj("files")).into_iter().map(|(id, obj)| self.read_file(id, &obj)).collect()
    }

    // ---- texts ------------------------------------------------------------------------

    pub fn add_text(&mut self, me: &str, name: &str, text: &str) -> String {
        let texts = self.obj("texts");
        let id = entry_key();
        let t = self.doc.put_object(&texts, id.as_str(), ObjType::Map).expect("put text");
        self.doc.put(&t, "from", me).unwrap();
        self.doc.put(&t, "name", name).unwrap();
        self.doc.put(&t, "text", text).unwrap();
        self.doc.put(&t, "ts", now_ms()).unwrap();
        id
    }

    pub fn delete_text(&mut self, id: &str) {
        let texts = self.obj("texts");
        let _ = self.doc.delete(&texts, id);
    }

    /// Oldest first.
    pub fn texts(&self) -> Vec<TextEntry> {
        self.entries(&self.obj("texts"))
            .into_iter()
            .map(|(id, t)| TextEntry {
                id,
                from: self.str_of(&t, "from"),
                name: self.str_of(&t, "name"),
                text: self.str_of(&t, "text"),
                ts: self.int_of(&t, "ts"),
            })
            .collect()
    }

    // ---- scratchpad -------------------------------------------------------------------

    pub fn scratch(&self) -> String {
        self.doc.text(self.obj("scratch")).unwrap_or_default()
    }

    /// Make the scratchpad read `new`, touching only the span that differs from the current text.
    /// Returns whether anything changed.
    pub fn set_scratch(&mut self, new: &str) -> bool {
        let obj = self.obj("scratch");
        let old = self.scratch();
        let (start, del, ins) = diff(&old, new);
        if del == 0 && ins.is_empty() {
            return false;
        }
        self.doc.splice_text(&obj, start, del as isize, &ins).expect("splice scratch");
        true
    }

    // ---- sync -------------------------------------------------------------------------

    pub fn heads(&mut self) -> Vec<automerge::ChangeHash> {
        self.doc.get_heads()
    }

    /// Forget what we knew about `peer`'s copy (it reconnected).
    pub fn reset_sync(&mut self, peer: &str) {
        self.sync.remove(peer);
    }

    /// The next sync message for `peer`, if there is something to say.
    pub fn sync_message(&mut self, peer: &str) -> Option<Vec<u8>> {
        let state = self.sync.entry(peer.to_owned()).or_default();
        self.doc.sync().generate_sync_message(state).map(|m| m.encode())
    }

    /// Apply a sync message from `peer`. Returns whether our document changed.
    pub fn receive_sync(&mut self, peer: &str, bytes: &[u8]) -> Result<bool, AutomergeError> {
        let message = sync::Message::decode(bytes).map_err(|e| {
            tracing::debug!("bad sync message: {e}");
            AutomergeError::Fail
        })?;
        let before = self.doc.get_heads();
        let state = self.sync.entry(peer.to_owned()).or_default();
        self.doc.sync().receive_sync_message(state, message)?;
        Ok(self.doc.get_heads() != before)
    }
}

/// `(start in chars, chars to delete, text to insert)` turning `old` into `new`.
pub fn diff(old: &str, new: &str) -> (usize, usize, String) {
    let (o, n): (Vec<char>, Vec<char>) = (old.chars().collect(), new.chars().collect());
    let prefix = o.iter().zip(&n).take_while(|(a, b)| a == b).count();
    let suffix = o[prefix..].iter().rev().zip(n[prefix..].iter().rev()).take_while(|(a, b)| a == b).count();
    (prefix, o.len() - prefix - suffix, n[prefix..n.len() - suffix].iter().collect())
}

/// Where a cursor at char offset `pos` in the old text ends up in the new one.
pub fn map_cursor(old: &str, new: &str, pos: usize) -> usize {
    let (start, del, ins) = diff(old, new);
    let ins = ins.chars().count();
    if pos <= start {
        pos
    } else if pos >= start + del {
        pos - del + ins
    } else {
        start + ins
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run the sync protocol between two docs until neither has anything to say.
    fn sync(a: &mut GroupDoc, b: &mut GroupDoc) {
        for _ in 0..20 {
            let mut quiet = true;
            if let Some(m) = a.sync_message("b") {
                b.receive_sync("a", &m).unwrap();
                quiet = false;
            }
            if let Some(m) = b.sync_message("a") {
                a.receive_sync("b", &m).unwrap();
                quiet = false;
            }
            if quiet {
                return;
            }
        }
        panic!("sync did not settle");
    }

    #[test]
    fn independent_genesis_merges() {
        let (mut a, mut b) = (GroupDoc::new(), GroupDoc::new());
        a.join("alice", "Alice's phone");
        b.join("bob", "Bob's laptop");
        sync(&mut a, &mut b);
        assert_eq!(a.members(), b.members());
        assert_eq!(a.members().len(), 2);
        // The root objects were not duplicated: both sides can still write to them and converge.
        a.add_text("alice", "Alice's phone", "https://example.com");
        b.add_text("bob", "Bob's laptop", "123456");
        sync(&mut a, &mut b);
        assert_eq!(a.texts(), b.texts());
        assert_eq!(a.texts().len(), 2);
    }

    #[test]
    fn files_track_downloads_and_stopping() {
        let (mut a, mut b) = (GroupDoc::new(), GroupDoc::new());
        let id = a.offer_file("alice", "A", "a.txt", 12, "abcd", "file");
        sync(&mut a, &mut b);
        b.mark_downloaded(&id, "bob");
        sync(&mut a, &mut b);
        let f = a.file(&id).unwrap();
        assert_eq!((f.name.as_str(), f.size, f.stopped), ("a.txt", 12, false));
        assert!(f.downloaded.contains_key("bob"));
        a.stop_file(&id);
        sync(&mut a, &mut b);
        assert!(b.file(&id).unwrap().stopped);
    }

    #[test]
    fn scratchpad_edits_merge_and_survive_save() {
        let (mut a, mut b) = (GroupDoc::new(), GroupDoc::new());
        a.set_scratch("héllo wörld");
        sync(&mut a, &mut b);
        a.set_scratch("héllo, wörld");
        b.set_scratch("héllo wörld!");
        sync(&mut a, &mut b);
        assert_eq!(a.scratch(), "héllo, wörld!");
        assert_eq!(b.scratch(), "héllo, wörld!");
        let bytes = a.save();
        assert_eq!(GroupDoc::load(&bytes).unwrap().scratch(), "héllo, wörld!");
    }

    #[test]
    fn diff_and_cursor() {
        assert_eq!(diff("abc", "abXc"), (2, 0, "X".to_string()));
        assert_eq!(diff("abc", "ac"), (1, 1, String::new()));
        assert_eq!(diff("same", "same"), (4, 0, String::new()));
        assert_eq!(map_cursor("abc", "abXc", 3), 4);
        assert_eq!(map_cursor("abc", "abXc", 1), 1);
        assert_eq!(map_cursor("abcdef", "af", 3), 1);
    }

    #[test]
    fn stale_sync_state_is_reset() {
        let (mut a, mut b) = (GroupDoc::new(), GroupDoc::new());
        a.set_scratch("one");
        sync(&mut a, &mut b);
        a.reset_sync("b");
        b.reset_sync("a");
        a.set_scratch("one two");
        sync(&mut a, &mut b);
        assert_eq!(b.scratch(), "one two");
    }
}
