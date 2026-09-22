//! Peer groups: a set of paired devices that share a list of files.
//!
//! A group has no name; it is told apart by who is in it, and carries a
//! random id only to name its document on disk. Its whole state is one
//! Automerge document that every member keeps in sync with every other online
//! member, so there is no history to order and no server to ask:
//!
//! ```text
//! members: { <endpoint id>: { state, invited_by, name } }
//! files:   { <file id>: { name, size, hash, owner, added_at,
//!                         holders: { <endpoint id>: true } } }
//! ```
//!
//! A file entry is a listing, not the bytes: they stay on the devices in
//! `holders`, and members download them on demand from whichever holder is
//! online. Anyone who has downloaded a file adds themselves as a holder.
//!
//! Automerge has no access control, so "only the owner removes their file"
//! and "you only change your own member state" are conventions of this
//! module's API, not guarantees against a hostile member.
//!
//! The document is created once, by the first member. Everyone else starts
//! from an empty document and receives it by sync: two devices creating the
//! `members` map independently would end up with two maps, one of them lost.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use automerge::sync::{self, SyncDoc};
use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value, ROOT};

use crate::protocol::identity::unix_now_ms;

const MEMBERS_KEY: &str = "members";
const FILES_KEY: &str = "files";
const HOLDERS_KEY: &str = "holders";

/// Where a member stands in the group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberState {
    /// Invited, has not answered yet.
    Pending,
    Active,
    Declined,
    Left,
}

impl MemberState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Declined => "declined",
            Self::Left => "left",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => Self::Pending,
            "active" => Self::Active,
            "declined" => Self::Declined,
            "left" => Self::Left,
            _ => return None,
        })
    }

    /// Members we keep a connection and a document sync with.
    pub fn is_connectable(self) -> bool {
        matches!(self, Self::Pending | Self::Active)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub endpoint_id: String,
    pub state: MemberState,
    pub invited_by: String,
    pub name: String,
}

/// What the owner announces about a file when sharing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFile {
    pub name: String,
    pub size: u64,
    /// BLAKE3 of the content, hex.
    pub hash: String,
    /// Where it is on this device, to serve it from.
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupFile {
    pub id: String,
    pub name: String,
    pub size: u64,
    pub hash: String,
    pub owner: String,
    pub added_at: u64,
    pub holders: Vec<String>,
    /// Our copy of it, if we have one (this part is not synced).
    pub local_path: Option<PathBuf>,
}

fn valid_endpoint_id(id: &str) -> bool {
    id.len() == 64 && id.chars().all(|c| c.is_ascii_hexdigit())
}

fn valid_group_id(id: &str) -> bool {
    id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit())
}

fn endpoint_key(id: &str) -> anyhow::Result<String> {
    anyhow::ensure!(valid_endpoint_id(id), "invalid endpoint id");
    Ok(id.to_lowercase())
}

fn get_obj(doc: &AutoCommit, parent: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(parent, key).ok()? {
        Some((Value::Object(ObjType::Map), id)) => Some(id),
        _ => None,
    }
}

fn get_str(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
    match doc.get(obj, key).ok()? {
        Some((Value::Scalar(v), _)) => match v.as_ref() {
            ScalarValue::Str(s) => Some(s.to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn get_u64(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<u64> {
    match doc.get(obj, key).ok()? {
        Some((Value::Scalar(v), _)) => match v.as_ref() {
            ScalarValue::Uint(n) => Some(*n),
            ScalarValue::Int(n) => u64::try_from(*n).ok(),
            _ => None,
        },
        _ => None,
    }
}

/// One group's document, its sync progress with each member, and its file.
pub struct GroupDoc {
    id: String,
    me: String,
    path: PathBuf,
    state: Mutex<DocState>,
    /// Where our copies of the group's files are, by file id. Local to this
    /// device, kept next to the document.
    local: Mutex<HashMap<String, PathBuf>>,
}

struct DocState {
    doc: AutoCommit,
    /// Sync progress per member, on that member's current connection.
    sync: HashMap<String, sync::State>,
}

impl GroupDoc {
    /// A new group of `me` and the already approved peer `first`, who gets an
    /// invitation (a pending member) to answer.
    pub fn create(
        dir: &Path,
        me: &str,
        my_name: &str,
        first: &str,
        first_name: &str,
    ) -> anyhow::Result<Self> {
        let me = endpoint_key(me)?;
        let first = endpoint_key(first)?;
        anyhow::ensure!(me != first, "cannot start a group with yourself");
        let id = uuid::Uuid::new_v4().simple().to_string();
        let mut doc = AutoCommit::new().with_actor(ActorId::random());
        doc.put_object(ROOT, MEMBERS_KEY, ObjType::Map)?;
        doc.put_object(ROOT, FILES_KEY, ObjType::Map)?;
        doc.commit();
        let this = Self::new(dir, id, me.clone(), doc)?;
        this.write_member(&me, MemberState::Active, &me, my_name)?;
        this.write_member(&first, MemberState::Pending, &me, first_name)?;
        this.rewrite()?;
        Ok(this)
    }

    /// A group we were invited to but have not received yet. It is empty
    /// until the inviter's sync messages arrive.
    pub fn join(dir: &Path, id: &str, me: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(valid_group_id(id), "invalid group id");
        let doc = AutoCommit::new().with_actor(ActorId::random());
        let this = Self::new(dir, id.to_lowercase(), endpoint_key(me)?, doc)?;
        this.rewrite()?;
        Ok(this)
    }

    /// Loads a group from its file in `dir`.
    pub fn load(dir: &Path, id: &str, me: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(valid_group_id(id), "invalid group id");
        let id = id.to_lowercase();
        let path = Self::path_of(dir, &id);
        let bytes = std::fs::read(&path).context("read group document")?;
        let mut doc = AutoCommit::load(&bytes).context("load group document")?;
        // A fresh actor per run: two runs never share an op counter.
        doc.set_actor(ActorId::random());
        let this = Self::new(dir, id, endpoint_key(me)?, doc)?;
        // Compact whatever incremental saves piled up.
        this.rewrite()?;
        Ok(this)
    }

    fn new(dir: &Path, id: String, me: String, doc: AutoCommit) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir).context("create group folder")?;
        let local = std::fs::read(Self::local_path_of(dir, &id))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Ok(Self {
            path: Self::path_of(dir, &id),
            id,
            me,
            state: Mutex::new(DocState {
                doc,
                sync: HashMap::new(),
            }),
            local: Mutex::new(local),
        })
    }

    fn path_of(dir: &Path, id: &str) -> PathBuf {
        dir.join(format!("{id}.automerge"))
    }

    fn local_path_of(dir: &Path, id: &str) -> PathBuf {
        dir.join(format!("{id}.files.json"))
    }

    /// Our copy of a file, if we have one and it is still there.
    pub fn local_path(&self, file_id: &str) -> Option<PathBuf> {
        self.local
            .lock()
            .expect("local lock")
            .get(file_id)
            .filter(|path| path.is_file())
            .cloned()
    }

    fn save_local(&self, local: &HashMap<String, PathBuf>) -> anyhow::Result<()> {
        let path = self.path.with_extension("files.json");
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(local)?).context("write group files")?;
        std::fs::rename(&tmp, &path).context("replace group files")?;
        Ok(())
    }

    /// Records where our copy of a file is.
    pub fn set_local_path(&self, file_id: &str, path: &Path) -> anyhow::Result<()> {
        let mut local = self.local.lock().expect("local lock");
        local.insert(file_id.to_string(), path.to_path_buf());
        self.save_local(&local)
    }

    /// Forgets our copy of a file (it stays on disk).
    pub fn clear_local_path(&self, file_id: &str) -> anyhow::Result<()> {
        let mut local = self.local.lock().expect("local lock");
        if local.remove(file_id).is_some() {
            self.save_local(&local)?;
        }
        Ok(())
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    fn rewrite(&self) -> anyhow::Result<()> {
        let bytes = self.state.lock().expect("group lock").doc.save();
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, bytes).context("write group document")?;
        std::fs::rename(&tmp, &self.path).context("replace group document")?;
        Ok(())
    }

    /// Appends what changed since the last save.
    fn append(&self, state: &mut DocState) -> anyhow::Result<()> {
        let bytes = state.doc.save_incremental();
        if bytes.is_empty() {
            return Ok(());
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .context("open group document")?;
        file.write_all(&bytes).context("append group document")?;
        Ok(())
    }

    /// Runs an edit, commits it and saves it.
    fn edit<T>(
        &self,
        f: impl FnOnce(&mut AutoCommit) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let mut state = self.state.lock().expect("group lock");
        let out = f(&mut state.doc)?;
        state.doc.commit();
        self.append(&mut state)?;
        Ok(out)
    }

    // ---- members ----

    fn write_member(
        &self,
        who: &str,
        member_state: MemberState,
        invited_by: &str,
        name: &str,
    ) -> anyhow::Result<()> {
        self.edit(|doc| {
            let members = get_obj(doc, &ROOT, MEMBERS_KEY).context("group not synced yet")?;
            let entry = match get_obj(doc, &members, who) {
                Some(entry) => entry,
                None => doc.put_object(&members, who, ObjType::Map)?,
            };
            doc.put(&entry, "state", member_state.as_str())?;
            doc.put(&entry, "invited_by", invited_by)?;
            doc.put(&entry, "name", name)?;
            Ok(())
        })
    }

    pub fn members(&self) -> Vec<Member> {
        let state = self.state.lock().expect("group lock");
        let doc = &state.doc;
        let Some(members) = get_obj(doc, &ROOT, MEMBERS_KEY) else {
            return Vec::new();
        };
        let mut out: Vec<Member> = doc
            .keys(&members)
            .filter_map(|who| {
                let entry = get_obj(doc, &members, &who)?;
                Some(Member {
                    state: MemberState::parse(&get_str(doc, &entry, "state")?)?,
                    invited_by: get_str(doc, &entry, "invited_by").unwrap_or_default(),
                    name: get_str(doc, &entry, "name").unwrap_or_default(),
                    endpoint_id: who,
                })
            })
            .collect();
        out.sort_by(|a, b| a.endpoint_id.cmp(&b.endpoint_id));
        out
    }

    /// Our own place in the group, once the document has told us.
    pub fn my_state(&self) -> Option<MemberState> {
        self.members()
            .into_iter()
            .find(|m| m.endpoint_id == self.me)
            .map(|m| m.state)
    }

    /// Invites `who`, who must be one of our approved peers. Anyone active in
    /// the group may do this. Someone already in the group is left alone,
    /// except that a member who declined or left is invited again.
    pub fn invite(&self, who: &str, name: &str) -> anyhow::Result<()> {
        let who = endpoint_key(who)?;
        anyhow::ensure!(
            self.my_state() == Some(MemberState::Active),
            "only members can invite"
        );
        let existing = self
            .members()
            .into_iter()
            .find(|m| m.endpoint_id == who)
            .map(|m| m.state);
        match existing {
            Some(MemberState::Pending | MemberState::Active) => Ok(()),
            _ => self.write_member(&who, MemberState::Pending, &self.me, name),
        }
    }

    /// Answers our own invitation.
    pub fn answer_invite(&self, accept: bool, my_name: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.my_state() == Some(MemberState::Pending),
            "no invitation to answer"
        );
        let invited_by = self
            .members()
            .into_iter()
            .find(|m| m.endpoint_id == self.me)
            .map(|m| m.invited_by)
            .unwrap_or_default();
        let new_state = if accept {
            MemberState::Active
        } else {
            MemberState::Declined
        };
        self.write_member(&self.me, new_state, &invited_by, my_name)
    }

    /// Leaves the group. The files others downloaded stay with them.
    pub fn leave(&self) -> anyhow::Result<()> {
        let me = self.me.clone();
        self.edit(|doc| {
            let members = get_obj(doc, &ROOT, MEMBERS_KEY).context("group not synced yet")?;
            let entry = get_obj(doc, &members, &me).context("not a member")?;
            doc.put(&entry, "state", MemberState::Left.as_str())?;
            Ok(())
        })?;
        // Our own listings and holdings are no longer reachable.
        for file in self.files() {
            if file.owner == self.me {
                self.remove_file(&file.id)?;
            } else {
                self.remove_holder(&file.id)?;
            }
        }
        Ok(())
    }

    /// Members to keep a connection to, other than ourselves. Nobody once we
    /// have left or declined, or before we know anything about the group.
    pub fn peers(&self) -> Vec<String> {
        if !matches!(
            self.my_state(),
            Some(MemberState::Pending | MemberState::Active)
        ) {
            return Vec::new();
        }
        self.members()
            .into_iter()
            .filter(|m| m.endpoint_id != self.me && m.state.is_connectable())
            .map(|m| m.endpoint_id)
            .collect()
    }

    // ---- files ----

    /// Lists a file we have, and returns its id. We are its first holder.
    pub fn add_file(&self, file: &NewFile) -> anyhow::Result<String> {
        anyhow::ensure!(
            self.my_state() == Some(MemberState::Active),
            "only members can share files"
        );
        anyhow::ensure!(!file.hash.is_empty(), "file has no hash");
        let id = uuid::Uuid::new_v4().simple().to_string();
        let me = self.me.clone();
        self.edit(|doc| {
            let files = get_obj(doc, &ROOT, FILES_KEY).context("group not synced yet")?;
            let entry = doc.put_object(&files, &id, ObjType::Map)?;
            doc.put(&entry, "name", file.name.as_str())?;
            doc.put(&entry, "size", file.size)?;
            doc.put(&entry, "hash", file.hash.as_str())?;
            doc.put(&entry, "owner", me.as_str())?;
            doc.put(&entry, "added_at", unix_now_ms())?;
            let holders = doc.put_object(&entry, HOLDERS_KEY, ObjType::Map)?;
            doc.put(&holders, me.as_str(), true)?;
            Ok(())
        })?;
        self.set_local_path(&id, &file.path)?;
        Ok(id)
    }

    /// Stops listing a file we own. Copies already downloaded stay put.
    pub fn remove_file(&self, file_id: &str) -> anyhow::Result<()> {
        let me = self.me.clone();
        self.edit(|doc| {
            let files = get_obj(doc, &ROOT, FILES_KEY).context("group not synced yet")?;
            let entry = get_obj(doc, &files, file_id).context("no such file")?;
            anyhow::ensure!(
                get_str(doc, &entry, "owner").as_deref() == Some(me.as_str()),
                "only the owner can remove a file"
            );
            doc.delete(&files, file_id)?;
            Ok(())
        })?;
        self.clear_local_path(file_id)
    }

    /// Records that we now have the file and can serve it.
    pub fn add_holder(&self, file_id: &str) -> anyhow::Result<()> {
        let me = self.me.clone();
        self.edit(|doc| {
            let holders = self.holders_obj(doc, file_id)?;
            doc.put(&holders, me.as_str(), true)?;
            Ok(())
        })
    }

    /// Records that we no longer have the file.
    pub fn remove_holder(&self, file_id: &str) -> anyhow::Result<()> {
        let me = self.me.clone();
        self.edit(|doc| {
            let holders = self.holders_obj(doc, file_id)?;
            if doc.get(&holders, me.as_str())?.is_some() {
                doc.delete(&holders, me.as_str())?;
            }
            Ok(())
        })?;
        self.clear_local_path(file_id)
    }

    fn holders_obj(&self, doc: &AutoCommit, file_id: &str) -> anyhow::Result<ObjId> {
        let files = get_obj(doc, &ROOT, FILES_KEY).context("group not synced yet")?;
        let entry = get_obj(doc, &files, file_id).context("no such file")?;
        get_obj(doc, &entry, HOLDERS_KEY).context("file has no holders list")
    }

    /// Files in the group, newest first. Holders who are not active members
    /// any more are left out.
    pub fn files(&self) -> Vec<GroupFile> {
        let active: BTreeMap<String, MemberState> = self
            .members()
            .into_iter()
            .map(|m| (m.endpoint_id, m.state))
            .collect();
        let state = self.state.lock().expect("group lock");
        let doc = &state.doc;
        let Some(files) = get_obj(doc, &ROOT, FILES_KEY) else {
            return Vec::new();
        };
        let mut out: Vec<GroupFile> = doc
            .keys(&files)
            .filter_map(|id| {
                let entry = get_obj(doc, &files, &id)?;
                let mut holders: Vec<String> = get_obj(doc, &entry, HOLDERS_KEY)
                    .map(|h| doc.keys(&h).collect())
                    .unwrap_or_default();
                holders.retain(|h| active.get(h) == Some(&MemberState::Active));
                holders.sort();
                Some(GroupFile {
                    name: get_str(doc, &entry, "name")?,
                    size: get_u64(doc, &entry, "size").unwrap_or(0),
                    hash: get_str(doc, &entry, "hash")?,
                    owner: get_str(doc, &entry, "owner").unwrap_or_default(),
                    added_at: get_u64(doc, &entry, "added_at").unwrap_or(0),
                    holders,
                    local_path: None,
                    id,
                })
            })
            .collect();
        drop(state);
        for file in &mut out {
            file.local_path = self.local_path(&file.id);
        }
        out.sort_by(|a, b| b.added_at.cmp(&a.added_at).then_with(|| a.id.cmp(&b.id)));
        out
    }

    // ---- sync ----

    /// The connection to `peer` changed: sync with it from scratch.
    pub fn reset_sync(&self, peer: &str) {
        self.state
            .lock()
            .expect("group lock")
            .sync
            .insert(peer.to_lowercase(), sync::State::new());
    }

    /// The next sync message for `peer`, if it is missing anything.
    pub fn generate_sync(&self, peer: &str) -> Option<Vec<u8>> {
        let mut guard = self.state.lock().expect("group lock");
        let state = &mut *guard;
        let sync_state = state.sync.entry(peer.to_lowercase()).or_default();
        let message = state.doc.sync().generate_sync_message(sync_state);
        message.map(|m| m.encode())
    }

    /// Applies a sync message from `peer`; true when the group changed.
    pub fn receive_sync(&self, peer: &str, bytes: &[u8]) -> anyhow::Result<bool> {
        let message = sync::Message::decode(bytes).context("decode group sync")?;
        let mut guard = self.state.lock().expect("group lock");
        let state = &mut *guard;
        let before = state.doc.get_heads();
        let sync_state = state.sync.entry(peer.to_lowercase()).or_default();
        state.doc.sync().receive_sync_message(sync_state, message)?;
        if state.doc.get_heads() == before {
            return Ok(false);
        }
        self.append(state)?;
        Ok(true)
    }
}

/// Every group on this device.
pub struct Groups {
    dir: PathBuf,
    me: String,
    groups: Mutex<HashMap<String, Arc<GroupDoc>>>,
}

impl Groups {
    /// Opens the groups stored under `data_dir`, as the device `me`.
    pub fn new(data_dir: &Path, me: &str) -> anyhow::Result<Self> {
        let this = Self {
            dir: data_dir.join("groups"),
            me: endpoint_key(me)?,
            groups: Mutex::new(HashMap::new()),
        };
        if let Ok(entries) = std::fs::read_dir(&this.dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(id) = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| n.strip_suffix(".automerge"))
                else {
                    continue;
                };
                if !valid_group_id(id) {
                    continue;
                }
                match GroupDoc::load(&this.dir, id, &this.me) {
                    Ok(group) => {
                        this.groups
                            .lock()
                            .expect("groups lock")
                            .insert(group.id.clone(), Arc::new(group));
                    }
                    // Keep what is there rather than overwrite it.
                    Err(err) => tracing::warn!("unreadable group {}: {err:#}", path.display()),
                }
            }
        }
        Ok(this)
    }

    pub fn list(&self) -> Vec<Arc<GroupDoc>> {
        let mut groups: Vec<_> = self.groups.lock().expect("groups lock").values().cloned().collect();
        groups.sort_by(|a, b| a.id.cmp(&b.id));
        groups
    }

    pub fn get(&self, id: &str) -> Option<Arc<GroupDoc>> {
        self.groups.lock().expect("groups lock").get(&id.to_lowercase()).cloned()
    }

    /// Starts a group with an approved peer.
    pub fn create(
        &self,
        my_name: &str,
        first: &str,
        first_name: &str,
    ) -> anyhow::Result<Arc<GroupDoc>> {
        let group = Arc::new(GroupDoc::create(
            &self.dir, &self.me, my_name, first, first_name,
        )?);
        self.groups
            .lock()
            .expect("groups lock")
            .insert(group.id.clone(), group.clone());
        Ok(group)
    }

    /// Makes room for a group we were invited to, whose document is still to
    /// arrive. Returns the existing one if we already know the group.
    pub fn join(&self, id: &str) -> anyhow::Result<Arc<GroupDoc>> {
        if let Some(group) = self.get(id) {
            return Ok(group);
        }
        let group = Arc::new(GroupDoc::join(&self.dir, id, &self.me)?);
        self.groups
            .lock()
            .expect("groups lock")
            .insert(group.id.clone(), group.clone());
        Ok(group)
    }

    /// Forgets a group and deletes its document, after we left it or turned
    /// down the invitation.
    pub fn remove(&self, id: &str) -> anyhow::Result<()> {
        let id = id.to_lowercase();
        if self.groups.lock().expect("groups lock").remove(&id).is_some() {
            std::fs::remove_file(GroupDoc::path_of(&self.dir, &id)).context("delete group")?;
            let _ = std::fs::remove_file(GroupDoc::local_path_of(&self.dir, &id));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    /// Runs the sync protocol between two devices' copies until quiet.
    fn sync_pair(a: &GroupDoc, b: &GroupDoc) {
        a.reset_sync(&b.me);
        b.reset_sync(&a.me);
        let mut turn_a = true;
        let mut idle = 0;
        while idle < 2 {
            let (from, to) = if turn_a { (a, b) } else { (b, a) };
            match from.generate_sync(&to.me) {
                Some(message) => {
                    idle = 0;
                    to.receive_sync(&from.me, &message).unwrap();
                }
                None => idle += 1,
            }
            turn_a = !turn_a;
        }
    }

    fn file(name: &str) -> NewFile {
        NewFile {
            name: name.into(),
            size: 10,
            hash: format!("hash-of-{name}"),
            path: PathBuf::from(format!("/nowhere/{name}")),
        }
    }

    /// A and B in a group, both active.
    fn pair(dir_a: &Path, dir_b: &Path) -> (GroupDoc, GroupDoc) {
        let a = GroupDoc::create(dir_a, A, "a", B, "b").unwrap();
        let b = GroupDoc::join(dir_b, a.id(), B).unwrap();
        sync_pair(&a, &b);
        assert_eq!(b.my_state(), Some(MemberState::Pending));
        b.answer_invite(true, "b").unwrap();
        sync_pair(&a, &b);
        (a, b)
    }

    #[test]
    fn invitation_reaches_the_invitee_and_is_accepted() {
        let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let a = GroupDoc::create(da.path(), A, "a", B, "b").unwrap();
        assert_eq!(a.my_state(), Some(MemberState::Active));
        assert_eq!(a.peers(), vec![B.to_string()]);

        let b = GroupDoc::join(db.path(), a.id(), B).unwrap();
        assert_eq!(b.my_state(), None);
        sync_pair(&a, &b);
        assert_eq!(b.my_state(), Some(MemberState::Pending));
        assert_eq!(b.members().len(), 2);

        b.answer_invite(true, "b").unwrap();
        sync_pair(&a, &b);
        let seen_by_a = a.members().into_iter().find(|m| m.endpoint_id == B).unwrap();
        assert_eq!(seen_by_a.state, MemberState::Active);
        assert_eq!(seen_by_a.invited_by, A);
    }

    #[test]
    fn declined_invitation_is_seen_and_can_be_repeated() {
        let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let a = GroupDoc::create(da.path(), A, "a", B, "b").unwrap();
        let b = GroupDoc::join(db.path(), a.id(), B).unwrap();
        sync_pair(&a, &b);
        b.answer_invite(false, "b").unwrap();
        sync_pair(&a, &b);
        assert!(a.peers().is_empty());
        a.invite(B, "b").unwrap();
        assert_eq!(a.peers(), vec![B.to_string()]);
    }

    #[test]
    fn any_member_can_invite_but_outsiders_cannot() {
        let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (a, b) = pair(da.path(), db.path());
        b.invite(C, "c").unwrap();
        sync_pair(&a, &b);
        let c = a.members().into_iter().find(|m| m.endpoint_id == C).unwrap();
        assert_eq!((c.state, c.invited_by.as_str()), (MemberState::Pending, B));

        // C has not accepted, so cannot invite or share yet.
        let dc = tempfile::tempdir().unwrap();
        let c = GroupDoc::join(dc.path(), a.id(), C).unwrap();
        sync_pair(&b, &c);
        assert!(c.invite(A, "a").is_err());
        assert!(c.add_file(&file("x")).is_err());
    }

    #[test]
    fn listed_file_gets_holders_that_merge() {
        let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (a, b) = pair(da.path(), db.path());

        let id = a.add_file(&file("notes.txt")).unwrap();
        sync_pair(&a, &b);
        let seen = b.files();
        assert_eq!(seen.len(), 1);
        assert_eq!((seen[0].name.as_str(), seen[0].owner.as_str()), ("notes.txt", A));
        assert_eq!(seen[0].holders, vec![A.to_string()]);

        b.add_holder(&id).unwrap();
        sync_pair(&a, &b);
        assert_eq!(a.files()[0].holders, vec![A.to_string(), B.to_string()]);

        b.remove_holder(&id).unwrap();
        sync_pair(&a, &b);
        assert_eq!(a.files()[0].holders, vec![A.to_string()]);
    }

    #[test]
    fn files_added_apart_both_survive() {
        let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (a, b) = pair(da.path(), db.path());
        a.add_file(&file("one")).unwrap();
        b.add_file(&file("two")).unwrap();
        sync_pair(&a, &b);
        assert_eq!(a.files().len(), 2);
        assert_eq!(b.files().len(), 2);
    }

    #[test]
    fn only_the_owner_removes_a_file() {
        let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (a, b) = pair(da.path(), db.path());
        let id = a.add_file(&file("mine")).unwrap();
        sync_pair(&a, &b);
        assert!(b.remove_file(&id).is_err());
        a.remove_file(&id).unwrap();
        sync_pair(&a, &b);
        assert!(b.files().is_empty());
    }

    #[test]
    fn leaving_withdraws_our_files_and_holdings() {
        let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (a, b) = pair(da.path(), db.path());
        let from_a = a.add_file(&file("a-file")).unwrap();
        b.add_file(&file("b-file")).unwrap();
        sync_pair(&a, &b);
        b.add_holder(&from_a).unwrap();
        b.leave().unwrap();
        sync_pair(&a, &b);

        assert_eq!(b.my_state(), Some(MemberState::Left));
        assert!(a.peers().is_empty());
        let files = a.files();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].holders, vec![A.to_string()]);
    }

    #[test]
    fn groups_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let id = {
            let groups = Groups::new(dir.path(), A).unwrap();
            let group = groups.create("a", B, "b").unwrap();
            group.add_file(&file("kept")).unwrap();
            group.id().to_string()
        };
        let groups = Groups::new(dir.path(), A).unwrap();
        let group = groups.get(&id).unwrap();
        assert_eq!(group.members().len(), 2);
        assert_eq!(group.files()[0].name, "kept");

        groups.remove(&id).unwrap();
        assert!(Groups::new(dir.path(), A).unwrap().list().is_empty());
    }

    #[test]
    fn our_copies_are_remembered_across_restarts_but_not_synced() {
        let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (a, b) = pair(da.path(), db.path());
        let real = da.path().join("real.txt");
        std::fs::write(&real, b"hi").unwrap();
        let id = a
            .add_file(&NewFile {
                path: real.clone(),
                ..file("real.txt")
            })
            .unwrap();
        sync_pair(&a, &b);
        assert_eq!(a.files()[0].local_path.as_deref(), Some(real.as_path()));
        assert_eq!(b.files()[0].local_path, None);

        let reopened = GroupDoc::load(da.path(), a.id(), A).unwrap();
        assert_eq!(reopened.local_path(&id).as_deref(), Some(real.as_path()));

        // A copy that is gone from disk is not a copy.
        std::fs::remove_file(&real).unwrap();
        assert_eq!(a.local_path(&id), None);

        // Removing the listing forgets the copy.
        std::fs::write(&real, b"hi").unwrap();
        a.set_local_path(&id, &real).unwrap();
        a.remove_file(&id).unwrap();
        assert_eq!(a.local_path(&id), None);
    }

    #[test]
    fn rejects_bad_input() {
        let dir = tempfile::tempdir().unwrap();
        assert!(GroupDoc::create(dir.path(), A, "a", A, "a").is_err());
        assert!(GroupDoc::create(dir.path(), A, "a", "../x", "x").is_err());
        assert!(GroupDoc::join(dir.path(), "../x", A).is_err());
        let a = GroupDoc::create(dir.path(), A, "a", B, "b").unwrap();
        assert!(a.receive_sync(B, b"nope").is_err());
    }
}
