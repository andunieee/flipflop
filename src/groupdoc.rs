//! The shared state of one group: an Automerge document holding its members, file offers, text
//! snippets and scratchpad. Pure data, no networking; [`crate::core`] moves the sync messages.
//!
//! Layout (all under the root map):
//! - `name`:   the group's name, as any member last set it (absent: the haiku name shows).
//! - `members`: peer id -> device name. Every peer only writes its own entry.
//! - `files`:   `<millis>-<rand>` -> { name, size, hash, kind, from, from_name, ts, stopped, stopped_at, downloaded: { peer id -> ts }, hosts: { peer id -> ts } }
//! - `music`:   the music tab's offers (of kind [`MUSIC_KIND`], or [`ALBUM_KIND`] for a folder, which
//!   also holds its `tracks`: [{ path, size, offset }]), shaped as in `files`. Apart from `files`
//!   so that peers from before the music tab neither list nor download them.
//! - `texts`:   `<millis>-<rand>` -> { from, name, text, ts }
//! - `scratch`: a text object everyone edits.
//! - `tabs`:    tab id -> { on }: which tabs the group shows, as any member set them. The ids are
//!   [`BUILTIN_TABS`] and apps' ids; an app's tab also holds { kind: "app", name, file, hash, size,
//!   from, ts }, `file` being the offer of its wasm in `files`. A built-in tab nobody touched shows
//!   unless it is one of [`OPTIONAL_TABS`].
//! - `apps`:    app id -> the app's own section, which only the app writes (see [`crate::apps`]).
//!
//! Entry keys start with the creation time, so key order is chronological.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use automerge::{
    ActorId, AutoCommit, AutomergeError, ChangeHash, ObjId, ObjType, ROOT, ReadDoc, Value,
    patches::Patch,
    sync,
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
    /// "file", "media", "apk", "folder", "wasm" (an app), or the music tab's [`MUSIC_KIND`] or
    /// [`ALBUM_KIND`].
    pub kind: String,
    /// Peer id of the offerer, who serves the bytes.
    pub from: String,
    pub from_name: String,
    pub ts: i64,
    pub stopped: bool,
    /// When the offerer stopped sharing (0: unknown, as for files stopped before this was recorded).
    pub stopped_at: i64,
    /// Peer id -> when they finished downloading.
    pub downloaded: BTreeMap<String, i64>,
    /// Peers (other than the offerer) that also serve the bytes. Each only writes its own entry.
    pub hosts: BTreeSet<String>,
    /// An album's songs, in the order they play; none for anything else.
    pub tracks: Vec<Track>,
}

/// A song in an album: a file of its folder.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    /// Within the folder, `/`-separated.
    pub path: String,
    pub size: u64,
    /// Where its bytes start in the folder's stream (its listing, then its files' bytes).
    pub offset: u64,
}

/// The tabs every group has, in the order they show; the apps' tabs follow, oldest first.
pub const BUILTIN_TABS: [&str; 4] = ["files", "texts", "pad", "music"];
/// The kind of the music tab's offers, which live in `music` instead of `files`.
pub const MUSIC_KIND: &str = "music";
/// The kind of a folder offered to the music tab: its songs play one by one.
pub const ALBUM_KIND: &str = "album";

/// An offer of the music tab, played and never downloaded.
pub fn is_music(kind: &str) -> bool {
    kind == MUSIC_KIND || kind == ALBUM_KIND
}
/// Built-in tabs that stay hidden until someone shows them.
pub const OPTIONAL_TABS: [&str; 1] = ["music"];

#[derive(Debug, Clone, PartialEq)]
pub struct Tab {
    /// One of [`BUILTIN_TABS`], or an app's id (also its section in `apps`).
    pub id: String,
    /// The group shows it. Built-in tabs do until someone hides them.
    pub on: bool,
    pub app: Option<AppTab>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppTab {
    pub name: String,
    /// The offer of its wasm in `files`.
    pub file: String,
    /// blake3 of the wasm, hex.
    pub hash: String,
    pub size: u64,
    /// Who added it.
    pub from: String,
    pub ts: i64,
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

#[cfg(not(target_arch = "wasm32"))]
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// This module is also compiled into the guest page (`web/`), where there is no system clock.
#[cfg(target_arch = "wasm32")]
pub fn now_ms() -> i64 {
    js_sys::Date::now() as i64
}

/// `n` random bytes, hex encoded. (Borrows iroh's key generator as a source of randomness.)
#[cfg(not(target_arch = "wasm32"))]
pub fn random_hex(n: usize) -> String {
    iroh::SecretKey::generate().to_bytes()[..n]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(target_arch = "wasm32")]
pub fn random_hex(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    getrandom::fill(&mut bytes).expect("browser randomness");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn entry_key() -> String {
    format!("{:013}-{}", now_ms(), random_hex(4))
}

/// The changes every copy starts from, built identically by every peer (fixed actor, no
/// timestamp), so independently created copies merge instead of fighting over the root objects.
fn genesis() -> AutoCommit {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![0u8; 16]));
    for (key, obj) in [
        ("members", ObjType::Map),
        ("files", ObjType::Map),
        ("texts", ObjType::Map),
        ("scratch", ObjType::Text),
    ] {
        doc.put_object(ROOT, key, obj).expect("genesis");
    }
    doc.commit_with(CommitOptions::default().with_time(0));
    // A change of its own, so documents made before apps existed can take it on in [`GroupDoc::load`].
    doc.put_object(ROOT, "tabs", ObjType::Map).expect("genesis");
    doc.put_object(ROOT, "apps", ObjType::Map).expect("genesis");
    doc.commit_with(CommitOptions::default().with_time(0));
    // Likewise for documents made before the music tab.
    doc.put_object(ROOT, "music", ObjType::Map)
        .expect("genesis");
    doc.commit_with(CommitOptions::default().with_time(0));
    doc
}

impl GroupDoc {
    pub fn new() -> GroupDoc {
        let mut doc = genesis();
        doc.set_actor(ActorId::random());
        GroupDoc {
            doc,
            sync: HashMap::new(),
        }
    }

    pub fn load(bytes: &[u8]) -> Result<GroupDoc, AutomergeError> {
        let mut doc = AutoCommit::load(bytes)?;
        if doc.get(ROOT, "music")?.is_none() {
            doc.merge(&mut genesis())?;
        }
        doc.set_actor(ActorId::random());
        Ok(GroupDoc {
            doc,
            sync: HashMap::new(),
        })
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

    // ---- name -------------------------------------------------------------------------

    /// The name the members gave the group, if any (else it shows its haiku name).
    pub fn name(&self) -> Option<String> {
        match self.doc.get(ROOT, "name") {
            Ok(Some((Value::Scalar(s), _))) => s
                .as_str()
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(str::to_owned),
            _ => None,
        }
    }

    /// Name the group for every member; `None` (or an empty name) goes back to the haiku.
    pub fn set_name(&mut self, name: Option<&str>) {
        let name = name.map(str::trim).filter(|n| !n.is_empty());
        match name {
            Some(name) => {
                if self.name().as_deref() != Some(name) {
                    self.doc.put(ROOT, "name", name).expect("put group name");
                }
            }
            None => {
                let _ = self.doc.delete(ROOT, "name");
            }
        }
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
            .filter_map(|item| {
                Some((
                    item.key.to_string(),
                    item.value.into_value().as_str()?.to_owned(),
                ))
            })
            .collect()
    }

    pub fn is_member(&self, peer: &str) -> bool {
        self.doc
            .get(self.obj("members"), peer)
            .ok()
            .flatten()
            .is_some()
    }

    // ---- files ------------------------------------------------------------------------

    pub fn offer_file(
        &mut self,
        me: &str,
        my_name: &str,
        name: &str,
        size: u64,
        hash: &str,
        kind: &str,
    ) -> String {
        let files = self.obj(if is_music(kind) { "music" } else { "files" });
        let id = entry_key();
        let f = self
            .doc
            .put_object(&files, id.as_str(), ObjType::Map)
            .expect("put file");
        self.doc.put(&f, "name", name).unwrap();
        self.doc.put(&f, "size", size as i64).unwrap();
        self.doc.put(&f, "hash", hash).unwrap();
        self.doc.put(&f, "kind", kind).unwrap();
        self.doc.put(&f, "from", me).unwrap();
        self.doc.put(&f, "from_name", my_name).unwrap();
        self.doc.put(&f, "ts", now_ms()).unwrap();
        self.doc.put(&f, "stopped", false).unwrap();
        self.doc.put_object(&f, "downloaded", ObjType::Map).unwrap();
        self.doc.put_object(&f, "hosts", ObjType::Map).unwrap();
        id
    }

    /// An offer in `files` or `music`.
    fn file_obj(&self, id: &str) -> Option<ObjId> {
        ["files", "music"]
            .into_iter()
            .find_map(|map| match self.doc.get(self.obj(map), id) {
                Ok(Some((Value::Object(_), obj))) => Some(obj),
                _ => None,
            })
    }

    /// Stop offering a file. The entry stays (so concurrent "downloaded" marks still merge).
    pub fn stop_file(&mut self, id: &str) {
        if let Some(f) = self.file_obj(id) {
            let _ = self.doc.put(&f, "stopped", true);
            let _ = self.doc.put(&f, "stopped_at", now_ms());
        }
    }

    /// Offer a stopped file again.
    pub fn unstop_file(&mut self, id: &str) {
        if let Some(f) = self.file_obj(id) {
            let _ = self.doc.put(&f, "stopped", false);
            let _ = self.doc.delete(&f, "stopped_at");
        }
    }

    pub fn mark_downloaded(&mut self, id: &str, me: &str) {
        let Some(f) = self.file_obj(id) else { return };
        if let Ok(Some((Value::Object(_), d))) = self.doc.get(&f, "downloaded") {
            let _ = self.doc.put(&d, me, now_ms());
        }
    }

    /// Start or stop serving a file's bytes as `me`.
    pub fn set_hosting(&mut self, id: &str, me: &str, on: bool) {
        let Some(f) = self.file_obj(id) else { return };
        let hosts = match self.doc.get(&f, "hosts") {
            Ok(Some((Value::Object(_), h))) => h,
            _ if on => self.doc.put_object(&f, "hosts", ObjType::Map).unwrap(),
            _ => return,
        };
        let _ = if on {
            self.doc.put(&hosts, me, now_ms())
        } else {
            self.doc.delete(&hosts, me)
        };
    }

    pub fn file(&self, id: &str) -> Option<FileOffer> {
        self.file_obj(id)
            .map(|obj| self.read_file(id.to_owned(), &obj))
    }

    /// In one pass over the offer's fields: a group's hundreds of songs are read on every change,
    /// and looking each field up on its own takes several times as long.
    fn read_file(&self, id: String, f: &ObjId) -> FileOffer {
        let mut offer = FileOffer {
            id,
            name: String::new(),
            size: 0,
            hash: String::new(),
            kind: String::new(),
            from: String::new(),
            from_name: String::new(),
            ts: 0,
            stopped: false,
            stopped_at: 0,
            downloaded: BTreeMap::new(),
            hosts: BTreeSet::new(),
            tracks: Vec::new(),
        };
        for item in self.doc.map_range(f, ..) {
            let key = item.key.as_ref();
            if let automerge::ValueRef::Object(ty) = item.value {
                let obj = item.id();
                match (key, ty) {
                    ("downloaded", _) => {
                        offer.downloaded = self
                            .doc
                            .map_range(&obj, ..)
                            .filter_map(|item| {
                                Some((item.key.to_string(), item.value.into_value().as_i64()?))
                            })
                            .collect()
                    }
                    ("hosts", _) => {
                        offer.hosts = self
                            .doc
                            .map_range(&obj, ..)
                            .map(|item| item.key.to_string())
                            .collect()
                    }
                    ("tracks", ObjType::List) => offer.tracks = self.tracks_in(&obj),
                    _ => {}
                }
                continue;
            }
            let value = item.value.into_value();
            let (text, int) = (
                || value.as_str().unwrap_or_default().to_owned(),
                || value.as_i64().unwrap_or(0),
            );
            match key {
                "name" => offer.name = text(),
                "size" => offer.size = int().max(0) as u64,
                "hash" => offer.hash = text(),
                "kind" => offer.kind = text(),
                "from" => offer.from = text(),
                "from_name" => offer.from_name = text(),
                "ts" => offer.ts = int(),
                "stopped" => offer.stopped = value.as_bool().unwrap_or(false),
                "stopped_at" => offer.stopped_at = int(),
                _ => {}
            }
        }
        offer
    }

    fn tracks_in(&self, list: &ObjId) -> Vec<Track> {
        (0..self.doc.length(list))
            .filter_map(|i| match self.doc.get(list, i) {
                Ok(Some((Value::Object(ObjType::Map), t))) => Some(Track {
                    path: self.str_of(&t, "path"),
                    size: self.int_of(&t, "size").max(0) as u64,
                    offset: self.int_of(&t, "offset").max(0) as u64,
                }),
                _ => None,
            })
            .collect()
    }

    /// Give an album its songs.
    pub fn set_tracks(&mut self, id: &str, tracks: &[Track]) {
        let Some(f) = self.file_obj(id) else { return };
        let list = self.doc.put_object(&f, "tracks", ObjType::List).unwrap();
        for (i, track) in tracks.iter().enumerate() {
            let t = self.doc.insert_object(&list, i, ObjType::Map).unwrap();
            self.doc.put(&t, "path", track.path.as_str()).unwrap();
            self.doc.put(&t, "size", track.size as i64).unwrap();
            self.doc.put(&t, "offset", track.offset as i64).unwrap();
        }
    }

    /// The files tab's offers (and apps'), oldest first.
    pub fn files(&self) -> Vec<FileOffer> {
        self.offers("files")
    }

    /// The music tab's offers, oldest first.
    pub fn music(&self) -> Vec<FileOffer> {
        self.offers("music")
    }

    /// The offers of `kind`'s tab: [`Self::music`] or [`Self::files`].
    pub fn offers_like(&self, kind: &str) -> Vec<FileOffer> {
        if is_music(kind) {
            self.music()
        } else {
            self.files()
        }
    }

    fn offers(&self, map: &str) -> Vec<FileOffer> {
        self.entries(&self.obj(map))
            .into_iter()
            .map(|(id, obj)| self.read_file(id, &obj))
            .collect()
    }

    // ---- texts ------------------------------------------------------------------------

    pub fn add_text(&mut self, me: &str, name: &str, text: &str) -> String {
        let texts = self.obj("texts");
        let id = entry_key();
        let t = self
            .doc
            .put_object(&texts, id.as_str(), ObjType::Map)
            .expect("put text");
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
        self.doc
            .splice_text(&obj, start, del as isize, &ins)
            .expect("splice scratch");
        true
    }

    // ---- tabs -------------------------------------------------------------------------

    fn tab_obj(&self, id: &str) -> Option<ObjId> {
        match self.doc.get(self.obj("tabs"), id) {
            Ok(Some((Value::Object(ObjType::Map), obj))) => Some(obj),
            _ => None,
        }
    }

    /// The built-in tabs, then the apps' oldest first.
    pub fn tabs(&self) -> Vec<Tab> {
        let builtin = BUILTIN_TABS.iter().map(|id| Tab {
            id: id.to_string(),
            on: self
                .tab_obj(id)
                .map_or(!OPTIONAL_TABS.contains(id), |t| self.bool_of(&t, "on")),
            app: None,
        });
        let apps = self
            .entries(&self.obj("tabs"))
            .into_iter()
            .filter(|(id, t)| {
                !BUILTIN_TABS.contains(&id.as_str()) && self.str_of(t, "kind") == "app"
            })
            .map(|(id, t)| Tab {
                id,
                on: self.bool_of(&t, "on"),
                app: Some(AppTab {
                    name: self.str_of(&t, "name"),
                    file: self.str_of(&t, "file"),
                    hash: self.str_of(&t, "hash"),
                    size: self.int_of(&t, "size").max(0) as u64,
                    from: self.str_of(&t, "from"),
                    ts: self.int_of(&t, "ts"),
                }),
            });
        builtin.chain(apps).collect()
    }

    /// Show or hide a tab. Returns whether there is such a tab.
    pub fn set_tab(&mut self, id: &str, on: bool) -> bool {
        let tab = match self.tab_obj(id) {
            Some(tab) => tab,
            // A built-in tab's entry is made the first time someone touches it.
            None if BUILTIN_TABS.contains(&id) => self
                .doc
                .put_object(self.obj("tabs"), id, ObjType::Map)
                .expect("put tab"),
            None => return false,
        };
        if self.bool_of(&tab, "on") != on || self.doc.get(&tab, "on").ok().flatten().is_none() {
            self.doc.put(&tab, "on", on).expect("put tab");
        }
        true
    }

    /// Add an app's tab, shown, for the wasm on offer as `file`. Returns the app's id.
    pub fn add_app_tab(
        &mut self,
        me: &str,
        name: &str,
        file: &str,
        hash: &str,
        size: u64,
    ) -> String {
        let id = entry_key();
        let t = self
            .doc
            .put_object(self.obj("tabs"), id.as_str(), ObjType::Map)
            .expect("put tab");
        self.doc.put(&t, "kind", "app").unwrap();
        self.doc.put(&t, "name", name).unwrap();
        self.doc.put(&t, "file", file).unwrap();
        self.doc.put(&t, "hash", hash).unwrap();
        self.doc.put(&t, "size", size as i64).unwrap();
        self.doc.put(&t, "from", me).unwrap();
        self.doc.put(&t, "ts", now_ms()).unwrap();
        self.doc.put(&t, "on", true).unwrap();
        id
    }

    // ---- apps -------------------------------------------------------------------------

    /// The app's section, made if it is not there yet. Every peer makes it with the identical change
    /// (an actor named after the app, on top of the genesis), so peers starting the same app at once
    /// share one section instead of one of them losing theirs. `None` if something else took its
    /// place.
    pub fn app_section(&mut self, app: &str) -> Option<ObjId> {
        let apps = self.obj("apps");
        if self.doc.get(&apps, app).ok()?.is_none() {
            let mut section = genesis();
            section.set_actor(ActorId::from(format!("app:{app}").into_bytes()));
            let apps = section.get(ROOT, "apps").ok()??.1;
            section.put_object(&apps, app, ObjType::Map).ok()?;
            section.commit_with(CommitOptions::default().with_time(0));
            self.doc.merge(&mut section).ok()?;
        }
        match self.doc.get(&apps, app) {
            Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
            _ => None,
        }
    }

    /// The raw document, for [`crate::apps`], which reads and writes app sections by path.
    pub fn raw(&mut self) -> &mut AutoCommit {
        &mut self.doc
    }

    /// What changed between two sets of heads.
    pub fn diff(&mut self, before: &[ChangeHash], after: &[ChangeHash]) -> Vec<Patch> {
        self.doc.diff(before, after)
    }

    // ---- sync -------------------------------------------------------------------------

    pub fn heads(&mut self) -> Vec<ChangeHash> {
        self.doc.get_heads()
    }

    /// Forget what we knew about `peer`'s copy (it reconnected).
    pub fn reset_sync(&mut self, peer: &str) {
        self.sync.remove(peer);
    }

    /// The next sync message for `peer`, if there is something to say.
    pub fn sync_message(&mut self, peer: &str) -> Option<Vec<u8>> {
        let state = self.sync.entry(peer.to_owned()).or_default();
        self.doc
            .sync()
            .generate_sync_message(state)
            .map(|m| m.encode())
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
    let suffix = o[prefix..]
        .iter()
        .rev()
        .zip(n[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    (
        prefix,
        o.len() - prefix - suffix,
        n[prefix..n.len() - suffix].iter().collect(),
    )
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
    fn the_name_is_shared() {
        let (mut a, mut b) = (GroupDoc::new(), GroupDoc::new());
        a.set_name(Some("weekend plans"));
        sync(&mut a, &mut b);
        assert_eq!(b.name().as_deref(), Some("weekend plans"));
        // Anyone can rename it, and clearing it brings back the haiku.
        b.set_name(Some("  "));
        sync(&mut a, &mut b);
        assert_eq!(a.name(), None);
        assert_eq!(b.name(), None);
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
    fn receivers_can_host_a_file() {
        let (mut a, mut b) = (GroupDoc::new(), GroupDoc::new());
        let id = a.offer_file("alice", "A", "a.txt", 12, "abcd", "file");
        sync(&mut a, &mut b);
        b.set_hosting(&id, "bob", true);
        sync(&mut a, &mut b);
        assert!(a.file(&id).unwrap().hosts.contains("bob"));
        b.set_hosting(&id, "bob", false);
        sync(&mut a, &mut b);
        assert!(a.file(&id).unwrap().hosts.is_empty());
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
    fn app_sections_made_at_once_are_one() {
        let (mut a, mut b) = (GroupDoc::new(), GroupDoc::new());
        let sa = a.app_section("chess").unwrap();
        let sb = b.app_section("chess").unwrap();
        a.raw().put(&sa, "from_a", 1).unwrap();
        b.raw().put(&sb, "from_b", 2).unwrap();
        sync(&mut a, &mut b);
        let s = a.app_section("chess").unwrap();
        assert_eq!(a.raw().keys(&s).collect::<Vec<_>>(), ["from_a", "from_b"]);
    }

    #[test]
    fn music_is_apart_from_files() {
        let (mut a, mut b) = (GroupDoc::new(), GroupDoc::new());
        let file = a.offer_file("alice", "A", "a.txt", 12, "abcd", "file");
        let song = a.offer_file("alice", "A", "a.mp3", 34, "ef01", MUSIC_KIND);
        sync(&mut a, &mut b);
        let ids = |offers: Vec<FileOffer>| offers.into_iter().map(|f| f.id).collect::<Vec<_>>();
        assert_eq!(ids(b.files()), [file]);
        assert_eq!(ids(b.music()), [song.clone()]);
        b.set_hosting(&song, "bob", true);
        a.stop_file(&song);
        sync(&mut a, &mut b);
        let f = a.file(&song).unwrap();
        assert!(f.stopped && f.hosts.contains("bob"));
        // What a peer from before the music tab reads.
        assert!(!a.doc.keys(a.obj("files")).any(|k| k == song));
    }

    #[test]
    fn albums_hold_their_tracks() {
        let (mut a, mut b) = (GroupDoc::new(), GroupDoc::new());
        let id = a.offer_file("alice", "A", "Album", 30, "ef01", ALBUM_KIND);
        let tracks = [
            Track {
                path: "1 one.mp3".into(),
                size: 10,
                offset: 40,
            },
            Track {
                path: "cd2/2 two.flac".into(),
                size: 20,
                offset: 50,
            },
        ];
        a.set_tracks(&id, &tracks);
        sync(&mut a, &mut b);
        let album = b.music().pop().unwrap();
        assert_eq!(
            (album.kind.as_str(), album.tracks.as_slice()),
            (ALBUM_KIND, &tracks[..])
        );
        assert!(b.files().is_empty());
        assert!(b.file(&id).unwrap().tracks.len() == 2);
    }

    #[test]
    fn old_documents_get_music() {
        // A document from before the music tab: the first two genesis changes only.
        let mut old = genesis();
        let changes: Vec<Vec<u8>> = old
            .get_changes(&[])
            .iter()
            .map(|c| c.raw_bytes().to_vec())
            .collect();
        let mut old = AutoCommit::new();
        old.load_incremental(&changes[..2].concat()).unwrap();
        assert!(old.get(ROOT, "music").unwrap().is_none());
        let (mut a, mut b) = (GroupDoc::load(&old.save()).unwrap(), GroupDoc::new());
        let id = a.offer_file("alice", "A", "a.mp3", 34, "ef01", MUSIC_KIND);
        sync(&mut a, &mut b);
        assert_eq!(b.music().len(), 1);
        assert_eq!(b.file(&id).unwrap().name, "a.mp3");
        assert_eq!(a.heads(), b.heads());
    }

    #[test]
    fn old_documents_get_apps() {
        // A document from before `apps`: the first genesis change only.
        let mut old = genesis();
        let first = old.get_changes(&[])[0].raw_bytes().to_vec();
        let mut old = AutoCommit::load(&first).unwrap();
        assert!(old.get(ROOT, "apps").unwrap().is_none());
        let (mut a, mut b) = (GroupDoc::load(&old.save()).unwrap(), GroupDoc::new());
        a.app_section("chess").unwrap();
        sync(&mut a, &mut b);
        assert!(b.app_section("chess").is_some());
        assert_eq!(a.heads(), b.heads());
    }

    #[test]
    fn tabs_are_shared() {
        let (mut a, mut b) = (GroupDoc::new(), GroupDoc::new());
        let on = |d: &GroupDoc| {
            d.tabs()
                .iter()
                .map(|t| (t.id.clone(), t.on))
                .collect::<Vec<_>>()
        };
        assert!(
            a.tabs()
                .iter()
                .all(|t| t.on != (t.id == "music") && t.app.is_none())
        );
        assert!(a.set_tab("texts", false));
        assert!(!a.set_tab("nope", false));
        let id = b.add_app_tab("bob", "chess.wasm", "f1", "abcd", 10);
        sync(&mut a, &mut b);
        assert_eq!(on(&a), on(&b));
        assert_eq!(
            on(&a),
            [
                ("files", true),
                ("texts", false),
                ("pad", true),
                ("music", false),
                (id.as_str(), true)
            ]
            .map(|(id, on)| (id.to_owned(), on))
        );
        a.set_tab(&id, false);
        b.set_tab("texts", true);
        b.set_tab("music", true);
        sync(&mut a, &mut b);
        assert_eq!(on(&b)[1..], on(&a)[1..]);
        assert!(!b.tabs()[4].on && b.tabs()[1].on && b.tabs()[3].on);
        assert_eq!(b.tabs()[4].app.as_ref().unwrap().file, "f1");
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
