//! What groups are for: files offered to the group, text snippets, a shared scratchpad. The shared
//! state is one Automerge document per group ([`GroupDoc`]), synced between its members over the
//! peer connections; file bytes travel straight from the offerer to whoever downloads them.
//!
//! Wire formats, next to the JSON control messages of [`super`]:
//! - sync: a uni stream holding `[SYNC_TAG, 16 raw group id bytes, automerge sync message]`;
//! - download: a bi stream; the downloader sends `{"group", "file"}` as JSON and the offerer answers
//!   `[status u8][length u64 BE][bytes…]` (status 0 = ok, 1 = refused).

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use iroh::{EndpointId, endpoint::{Connection, RecvStream, SendStream}};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use super::{Core, Event, parse_gid};
use crate::{
    groupdoc::GroupDoc,
    state::{MemberState, Share},
};

pub(super) const SYNC_TAG: u8 = 1;
const CHUNK: usize = 64 * 1024;
const MAX_TEXT_BYTES: usize = 256 * 1024;
const RETRY_AFTER: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq)]
pub struct DocMember {
    pub id: String,
    pub name: String,
    pub me: bool,
    pub online: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileView {
    pub id: String,
    pub name: String,
    pub size: u64,
    /// "file", "media" or "apk".
    pub kind: String,
    pub from: String,
    pub from_name: String,
    /// We are the one sharing it.
    pub mine: bool,
    pub ts: i64,
    /// The offerer stopped sharing it.
    pub stopped: bool,
    /// Names of the members that have it (the offerer excluded).
    pub downloaded_by: Vec<String>,
    /// Members other than the offerer: who it is for.
    pub audience: usize,
    /// We hold a copy (we offered it, or downloaded it).
    pub have: bool,
    /// Where our downloaded copy is.
    pub saved_to: Option<PathBuf>,
    /// Download in progress: (bytes so far, total).
    pub progress: Option<(u64, u64)>,
    pub error: Option<String>,
    pub offerer_online: bool,
    /// Names of the members (not the offerer) that also serve it.
    pub hosts: Vec<String>,
    /// We serve it too.
    pub hosting: bool,
    /// Someone who has it is online: the offerer or a host (we ourselves excluded).
    pub source_online: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextView {
    pub id: String,
    pub from: String,
    /// Device name of the sender when it was sent.
    pub name: String,
    pub text: String,
    pub ts: i64,
    pub mine: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct GroupView {
    pub id: String,
    pub members: Vec<DocMember>,
    /// Newest first.
    pub files: Vec<FileView>,
    /// Newest first.
    pub texts: Vec<TextView>,
    pub scratch: String,
    /// The scratchpad changed because of our own edit (the editor already shows it).
    pub scratch_local: bool,
}

struct Transfer {
    done: u64,
    total: u64,
    active: bool,
    error: Option<String>,
    failed_at: Option<Instant>,
}

pub(super) struct Sharing {
    dir: PathBuf,
    docs: Mutex<HashMap<String, GroupDoc>>,
    dirty: Mutex<HashSet<String>>,
    transfers: Mutex<HashMap<(String, String), Transfer>>,
    download_dir: Mutex<PathBuf>,
    /// What the UI was last told per group, so unchanged views are not sent again.
    shown: Mutex<HashMap<String, GroupView>>,
    /// Per group: the scratchpad text (hashed) as of our own last edit, which is not news to us.
    pub(super) own_pad: Mutex<HashMap<String, String>>,
    /// The news last sent to the UI.
    pub(super) home: Mutex<Vec<super::Unseen>>,
}

impl Sharing {
    pub(super) fn new(data_dir: &Path) -> Sharing {
        let dir = data_dir.join("groups");
        let _ = std::fs::create_dir_all(&dir);
        #[cfg(not(target_os = "android"))]
        let download_dir = dirs::download_dir().unwrap_or_else(|| data_dir.join("downloads")).join("flipflop");
        #[cfg(target_os = "android")]
        let download_dir = data_dir.join("downloads");
        Sharing {
            dir,
            docs: Mutex::default(),
            dirty: Mutex::default(),
            transfers: Mutex::default(),
            download_dir: Mutex::new(download_dir),
            shown: Mutex::default(),
            own_pad: Mutex::default(),
            home: Mutex::default(),
        }
    }

    fn doc_path(&self, gid: &str) -> PathBuf {
        self.dir.join(format!("{gid}.am"))
    }
}

#[derive(Serialize, Deserialize)]
struct DownloadRequest {
    group: String,
    file: String,
}

impl Core {
    fn me(&self) -> String {
        self.id().to_string()
    }

    /// Where downloads go.
    pub fn download_dir(&self) -> PathBuf {
        self.0.sharing.download_dir.lock().unwrap().clone()
    }

    pub fn set_download_dir(&self, dir: PathBuf) {
        *self.0.sharing.download_dir.lock().unwrap() = dir;
    }

    fn has_group(&self, gid: &str) -> bool {
        self.0.store.lock().unwrap().state.groups.iter().any(|g| g.id == gid)
    }

    fn require_group(&self, gid: &str) -> Result<()> {
        self.has_group(gid).then_some(()).ok_or_else(|| anyhow!("no such group"))
    }

    // ---- documents --------------------------------------------------------------------

    /// Run `f` on the group's document, loading it from disk (or starting it) first.
    pub(super) fn with_doc<R>(&self, gid: &str, f: impl FnOnce(&mut GroupDoc) -> R) -> R {
        let sharing = &self.0.sharing;
        let mut docs = sharing.docs.lock().unwrap();
        let doc = docs.entry(gid.to_owned()).or_insert_with(|| {
            std::fs::read(sharing.doc_path(gid))
                .ok()
                .and_then(|b| GroupDoc::load(&b).map_err(|e| tracing::warn!("group document unreadable: {e}")).ok())
                .unwrap_or_else(GroupDoc::new)
        });
        f(doc)
    }

    /// Load every group's document, announce ourselves in it and keep it saved.
    pub(super) fn load_docs(&self) {
        let groups: Vec<String> = self.0.store.lock().unwrap().state.groups.iter().map(|g| g.id.clone()).collect();
        for gid in groups {
            self.join_doc(&gid);
        }
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                this.save_docs();
            }
        });
    }

    pub(super) fn save_docs(&self) {
        let dirty: Vec<String> = self.0.sharing.dirty.lock().unwrap().drain().collect();
        for gid in dirty {
            if !self.has_group(&gid) {
                continue;
            }
            let bytes = self.with_doc(&gid, |d| d.save());
            let path = self.0.sharing.doc_path(&gid);
            let tmp = path.with_extension("am.tmp");
            if let Err(err) = std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, &path)) {
                tracing::warn!("could not save group document: {err}");
            }
        }
    }

    /// Add ourselves to the group's member list.
    pub(super) fn join_doc(&self, gid: &str) {
        let (me, name) = (self.me(), self.name());
        self.with_doc(gid, |d| d.join(&me, &name));
        self.doc_changed(gid, false);
    }

    pub(super) fn refresh_doc_names(&self) {
        let groups: Vec<String> = self.0.sharing.docs.lock().unwrap().keys().cloned().collect();
        for gid in groups {
            if self.has_group(&gid) {
                self.join_doc(&gid);
            }
        }
    }

    pub(super) fn drop_doc(&self, gid: &str) {
        self.0.sharing.docs.lock().unwrap().remove(gid);
        self.0.sharing.dirty.lock().unwrap().remove(gid);
        let _ = std::fs::remove_file(self.0.sharing.doc_path(gid));
        let gone: Vec<Share> = self.with_state(|st| {
            let (gone, keep) = std::mem::take(&mut st.shares).into_iter().partition(|s| s.group == gid);
            st.shares = keep;
            gone
        });
        self.release_sources(gone);
    }

    /// Everyone entitled to this group's documents: those who accepted our invite, and the members
    /// the document itself lists.
    fn group_peers(&self, gid: &str) -> Vec<EndpointId> {
        let mut ids: BTreeSet<String> = {
            let store = self.0.store.lock().unwrap();
            store
                .state
                .groups
                .iter()
                .filter(|g| g.id == gid)
                .flat_map(|g| g.members.iter().filter(|m| m.state == MemberState::Accepted).map(|m| m.peer.clone()))
                .collect()
        };
        ids.extend(self.with_doc(gid, |d| d.members()).into_iter().map(|(id, _)| id));
        let me = self.id();
        ids.iter().filter_map(|id| id.parse().ok()).filter(|id| *id != me).collect()
    }

    /// Everyone any of our groups' documents lists as a member.
    pub(super) fn doc_peers(&self) -> Vec<EndpointId> {
        let me = self.id();
        let docs = self.0.sharing.docs.lock().unwrap();
        let ids: BTreeSet<String> = docs.values().flat_map(|d| d.members()).map(|(id, _)| id).collect();
        ids.iter().filter_map(|id| id.parse().ok()).filter(|id| *id != me).collect()
    }

    fn connection(&self, peer: EndpointId) -> Option<Connection> {
        self.0.peers.lock().unwrap().get(&peer).and_then(|p| p.conn.clone()).filter(|c| c.close_reason().is_none())
    }

    /// Something in the document changed: save, show, sync and react.
    pub(super) fn doc_changed(&self, gid: &str, scratch_local: bool) {
        self.0.sharing.dirty.lock().unwrap().insert(gid.to_owned());
        self.publish_group(gid, scratch_local);
        self.sync_soon(gid);
        self.auto_jobs(gid);
    }

    // ---- views ------------------------------------------------------------------------

    pub fn group_view(&self, gid: &str) -> Option<GroupView> {
        self.has_group(gid).then(|| self.build_view(gid, false))
    }

    pub(super) fn build_view(&self, gid: &str, scratch_local: bool) -> GroupView {
        let me = self.me();
        let (members, files, texts, scratch) =
            self.with_doc(gid, |d| (d.members(), d.files(), d.texts(), d.scratch()));
        let online: HashSet<String> = self
            .0
            .peers
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, p)| p.conn.is_some())
            .map(|(id, _)| id.to_string())
            .chain(self.guests_online(gid))
            .collect();
        let received: HashMap<String, PathBuf> = self
            .0
            .store
            .lock()
            .unwrap()
            .state
            .received
            .iter()
            .filter(|r| r.group == gid)
            .map(|r| (r.file.clone(), r.path.clone()))
            .collect();
        let transfers = self.0.sharing.transfers.lock().unwrap();
        let name_of = |id: &str| members.iter().find(|(m, _)| m == id).map_or_else(|| id.chars().take(8).collect(), |(_, n)| n.clone());

        let mut file_views: Vec<FileView> = files
            .iter()
            .map(|f| {
                let mine = f.from == me;
                let t = transfers.get(&(gid.to_owned(), f.id.clone()));
                let audience = members.iter().filter(|(id, _)| *id != f.from).count();
                FileView {
                    id: f.id.clone(),
                    name: f.name.clone(),
                    size: f.size,
                    kind: f.kind.clone(),
                    from: f.from.clone(),
                    from_name: if f.from_name.is_empty() { name_of(&f.from) } else { f.from_name.clone() },
                    mine,
                    ts: f.ts,
                    stopped: f.stopped,
                    downloaded_by: f.downloaded.keys().filter(|id| **id != f.from).map(|id| name_of(id)).collect(),
                    audience,
                    have: mine || f.downloaded.contains_key(&me),
                    saved_to: received.get(&f.id).cloned(),
                    progress: t.filter(|t| t.active).map(|t| (t.done, t.total)),
                    error: t.and_then(|t| t.error.clone()),
                    offerer_online: online.contains(&f.from),
                    hosts: f.hosts.iter().filter(|id| **id != f.from).map(|id| name_of(id)).collect(),
                    hosting: f.hosts.contains(&me),
                    source_online: online.contains(&f.from) || f.hosts.iter().any(|h| *h != me && online.contains(h)),
                }
            })
            .collect();
        file_views.reverse();
        let mut text_views: Vec<TextView> = texts
            .into_iter()
            .map(|t| TextView { mine: t.from == me, id: t.id, from: t.from, name: t.name, text: t.text, ts: t.ts })
            .collect();
        text_views.reverse();
        GroupView {
            id: gid.to_owned(),
            members: members
                .iter()
                .map(|(id, name)| DocMember { id: id.clone(), name: name.clone(), me: *id == me, online: online.contains(id) })
                .collect(),
            files: file_views,
            texts: text_views,
            scratch,
            scratch_local,
        }
    }

    pub(super) fn publish_group(&self, gid: &str, scratch_local: bool) {
        let _ordered = self.0.publish_lock.lock().unwrap();
        self.emit_group(gid, scratch_local, true);
        self.emit_home();
    }

    /// Emit a group's view if it differs from the last one sent (or `force`). The caller holds
    /// the publish lock.
    fn emit_group(&self, gid: &str, scratch_local: bool, force: bool) {
        if !self.has_group(gid) {
            self.0.sharing.shown.lock().unwrap().remove(gid);
            return;
        }
        let view = self.build_view(gid, scratch_local);
        let mut shown = self.0.sharing.shown.lock().unwrap();
        if !force && shown.get(gid) == Some(&view) {
            return;
        }
        shown.insert(gid.to_owned(), view.clone());
        drop(shown);
        self.emit(Event::Group(view));
    }

    /// Peers came or went: members' online marks may have changed. Caller holds the publish lock.
    pub(super) fn refresh_group_views(&self) {
        let ids: Vec<String> = self.0.store.lock().unwrap().state.groups.iter().map(|g| g.id.clone()).collect();
        for gid in ids {
            self.emit_group(&gid, false, false);
        }
    }

    /// Re-send every group's view (the UI just started).
    pub fn publish_groups(&self) {
        let _ordered = self.0.publish_lock.lock().unwrap();
        let ids: Vec<String> = self.0.store.lock().unwrap().state.groups.iter().map(|g| g.id.clone()).collect();
        for gid in ids {
            self.emit_group(&gid, false, true);
        }
        self.0.sharing.home.lock().unwrap().clear();
        self.emit_home();
    }

    // ---- sync -------------------------------------------------------------------------

    pub(super) fn reset_doc_sync(&self, peer: EndpointId) {
        let key = peer.to_string();
        for doc in self.0.sharing.docs.lock().unwrap().values_mut() {
            doc.reset_sync(&key);
        }
    }

    pub(super) fn on_sync(&self, from: EndpointId, bytes: &[u8]) {
        if bytes.len() < 16 {
            return;
        }
        let gid: String = bytes[..16].iter().map(|b| format!("{b:02x}")).collect();
        if !self.has_group(&gid) || !self.group_peers(&gid).contains(&from) {
            tracing::debug!("ignoring sync for {gid} from {}", from.fmt_short());
            return;
        }
        let key = from.to_string();
        match self.with_doc(&gid, |d| d.receive_sync(&key, &bytes[16..])) {
            Ok(true) => self.doc_changed(&gid, false),
            Ok(false) => self.sync_peer_soon(&gid, from),
            Err(err) => tracing::debug!("sync from {} failed: {err}", from.fmt_short()),
        }
    }

    /// Say what we have to one peer about one group (it just accepted, or answered us).
    pub(super) fn sync_peer_soon(&self, gid: &str, peer: EndpointId) {
        let (this, gid) = (self.clone(), gid.to_owned());
        tokio::spawn(async move { this.sync_one(&gid, peer).await });
    }

    fn sync_soon(&self, gid: &str) {
        self.sync_guests(gid);
        let (this, gid) = (self.clone(), gid.to_owned());
        tokio::spawn(async move {
            for peer in this.group_peers(&gid) {
                this.sync_one(&gid, peer).await;
            }
        });
    }

    /// All groups, one peer.
    pub(super) async fn sync_peer(&self, peer: EndpointId) {
        let groups: Vec<String> = self.0.store.lock().unwrap().state.groups.iter().map(|g| g.id.clone()).collect();
        for gid in groups {
            if self.group_peers(&gid).contains(&peer) {
                self.sync_one(&gid, peer).await;
            }
        }
    }

    /// Send `peer` what it is missing of one group. A message that cannot be delivered (the
    /// connection was just replaced or dropped) must not count as sent, so then the peer's sync
    /// state starts over and the next attempt says everything again.
    async fn sync_one(&self, gid: &str, peer: EndpointId) {
        let key = peer.to_string();
        let Some(raw) = parse_gid(gid) else { return };
        for _ in 0..16 {
            let Some(conn) = self.connection(peer) else { return };
            let Some(message) = self.with_doc(gid, |d| d.sync_message(&key)) else { break };
            let mut bytes = vec![SYNC_TAG];
            bytes.extend_from_slice(&raw);
            bytes.extend_from_slice(&message);
            if let Err(err) = send_bytes(&conn, &bytes).await {
                tracing::debug!("sync to {} failed: {err:#}", peer.fmt_short());
                self.with_doc(gid, |d| d.reset_sync(&key));
                break;
            }
        }
    }

    // ---- texts and scratchpad ---------------------------------------------------------

    /// Post a snippet (a link, a code, anything) to the group, labelled with this device's name.
    pub fn send_text(&self, gid: &str, text: &str) -> Result<()> {
        self.require_group(gid)?;
        let text = text.trim();
        if text.is_empty() {
            bail!("nothing to send");
        }
        if text.len() > MAX_TEXT_BYTES {
            bail!("too long");
        }
        let (me, name) = (self.me(), self.name());
        self.with_doc(gid, |d| d.add_text(&me, &name, text));
        self.doc_changed(gid, false);
        Ok(())
    }

    /// Remove one of our own snippets.
    pub fn delete_text(&self, gid: &str, text_id: &str) -> Result<()> {
        self.require_group(gid)?;
        let me = self.me();
        let mine = self.with_doc(gid, |d| d.texts().iter().any(|t| t.id == text_id && t.from == me));
        if !mine {
            bail!("not your snippet");
        }
        self.with_doc(gid, |d| d.delete_text(text_id));
        self.doc_changed(gid, false);
        Ok(())
    }

    pub fn scratch(&self, gid: &str) -> Option<String> {
        self.has_group(gid).then(|| self.with_doc(gid, |d| d.scratch()))
    }

    /// Replace the scratchpad text; only the differing span is edited, so concurrent edits merge.
    pub fn set_scratch(&self, gid: &str, text: &str) {
        if !self.has_group(gid) {
            return;
        }
        if self.with_doc(gid, |d| d.set_scratch(text)) {
            let now = self.with_doc(gid, |d| d.scratch());
            self.0.sharing.own_pad.lock().unwrap().insert(gid.to_owned(), super::unseen::hash(&now));
            self.doc_changed(gid, true);
        }
    }

    // ---- files ------------------------------------------------------------------------

    /// Offer a file to the group, under `name` (default: the file's own). `staged`: the file at `path` is our own copy (made for the offer),
    /// removed again when sharing stops. `kind` is "file", "media" or "apk".
    pub async fn offer_file(&self, gid: &str, path: PathBuf, name: Option<String>, kind: &str, staged: bool) -> Result<String> {
        self.require_group(gid)?;
        let name = match name {
            Some(name) => clean_file_name(&name),
            None => path.file_name().map(|n| n.to_string_lossy().into_owned()).context("that path has no file name")?,
        };
        let hashing = path.clone();
        let (hash, size) = tokio::task::spawn_blocking(move || -> Result<(String, u64)> {
            let mut file = open_source(&hashing).with_context(|| format!("cannot read {}", hashing.display()))?;
            if !file.metadata()?.is_file() {
                bail!("{} is not a file", hashing.display());
            }
            let mut hasher = blake3::Hasher::new();
            let size = std::io::copy(&mut file, &mut hasher)?;
            Ok((hasher.finalize().to_hex().to_string(), size))
        })
        .await??;
        let (me, my_name) = (self.me(), self.name());
        let fid = self.with_doc(gid, |d| d.offer_file(&me, &my_name, &name, size, &hash, kind));
        self.with_state(|st| st.shares.push(Share { group: gid.to_owned(), file: fid.clone(), path, staged }));
        self.doc_changed(gid, false);
        Ok(fid)
    }

    /// A guest uploaded a file (now at `path`): offer it to the group in their name, served from here.
    pub(super) fn add_guest_offer(&self, gid: &str, guest: &str, guest_name: &str, path: PathBuf, name: &str, size: u64, hash: &str) -> String {
        let me = self.me();
        let fid = self.with_doc(gid, |d| {
            let fid = d.offer_file(guest, guest_name, name, size, hash, "file");
            d.set_hosting(&fid, &me, true);
            d.mark_downloaded(&fid, &me);
            fid
        });
        self.with_state(|st| st.shares.push(Share { group: gid.to_owned(), file: fid.clone(), path, staged: false }));
        self.doc_changed(gid, false);
        fid
    }

    /// Stop offering one of our files; nobody can download it afterwards.
    pub fn stop_file(&self, gid: &str, file: &str) -> Result<()> {
        self.require_group(gid)?;
        let me = self.me();
        // A guest's upload is offered in their name, but we are the one serving it.
        if !self.with_doc(gid, |d| d.file(file).is_some_and(|f| f.from == me || (super::guest::is_guest(&f.from) && f.hosts.contains(&me)))) {
            bail!("you are not sharing that file");
        }
        self.with_doc(gid, |d| d.stop_file(file));
        let gone = self.with_state(|st| {
            let (gone, keep) = std::mem::take(&mut st.shares).into_iter().partition::<Vec<_>, _>(|s| s.group == gid && s.file == file);
            st.shares = keep;
            gone
        });
        self.release_sources(gone);
        self.doc_changed(gid, false);
        Ok(())
    }

    /// Serve a file we downloaded to the rest of the group too, so it stays available when the
    /// offerer is offline. Hosting ends when the offerer stops sharing, or with [`Core::unhost_file`].
    pub fn host_file(&self, gid: &str, fid: &str) -> Result<()> {
        self.require_group(gid)?;
        let me = self.me();
        let offer = self.with_doc(gid, |d| d.file(fid)).context("no such file")?;
        if offer.from == me {
            bail!("that is your own file");
        }
        if offer.stopped {
            bail!("the sender stopped sharing it");
        }
        let path = self
            .0
            .store
            .lock()
            .unwrap()
            .state
            .received
            .iter()
            .find(|r| r.group == gid && r.file == fid && r.path.exists())
            .map(|r| r.path.clone())
            .context("download it first")?;
        self.with_state(|st| {
            if !st.shares.iter().any(|s| s.group == gid && s.file == fid) {
                st.shares.push(Share { group: gid.to_owned(), file: fid.to_owned(), path, staged: false });
            }
        });
        self.with_doc(gid, |d| d.set_hosting(fid, &me, true));
        self.doc_changed(gid, false);
        Ok(())
    }

    pub fn unhost_file(&self, gid: &str, fid: &str) -> Result<()> {
        self.require_group(gid)?;
        let me = self.me();
        if self.with_doc(gid, |d| d.file(fid)).is_some_and(|f| f.from == me) {
            bail!("that is your own file");
        }
        self.with_state(|st| st.shares.retain(|s| !(s.group == gid && s.file == fid)));
        self.with_doc(gid, |d| d.set_hosting(fid, &me, false));
        self.doc_changed(gid, false);
        Ok(())
    }

    fn begin_transfer(&self, gid: &str, fid: &str, total: u64, manual: bool) -> bool {
        let mut transfers = self.0.sharing.transfers.lock().unwrap();
        let key = (gid.to_owned(), fid.to_owned());
        if let Some(t) = transfers.get(&key) {
            let retry_ok = manual || t.failed_at.is_none_or(|at| at.elapsed() >= RETRY_AFTER);
            if t.active || !retry_ok {
                return false;
            }
        }
        transfers.insert(key, Transfer { done: 0, total, active: true, error: None, failed_at: None });
        true
    }

    /// Download an offered file into [`Core::download_dir`]; returns where it went. Resolves when
    /// done. Progress and errors show up in the group's [`FileView`].
    pub async fn download_file(&self, gid: &str, fid: &str) -> Result<PathBuf> {
        self.download(gid, fid, true).await
    }

    async fn download(&self, gid: &str, fid: &str, manual: bool) -> Result<PathBuf> {
        self.require_group(gid)?;
        let offer = self.with_doc(gid, |d| d.file(fid)).context("no such file")?;
        let me = self.me();
        if offer.from == me {
            bail!("that is your own file");
        }
        if offer.stopped {
            bail!("the sender stopped sharing it");
        }
        if offer.downloaded.contains_key(&me) && self.0.store.lock().unwrap().state.received.iter().any(|r| r.file == fid && r.path.exists()) {
            bail!("already downloaded");
        }
        if !self.begin_transfer(gid, fid, offer.size, manual) {
            bail!("already downloading");
        }
        self.publish_group(gid, false);
        let result = self.fetch(gid, fid, &offer).await;
        {
            let mut transfers = self.0.sharing.transfers.lock().unwrap();
            match &result {
                Ok(_) => {
                    transfers.remove(&(gid.to_owned(), fid.to_owned()));
                }
                Err(err) => {
                    if let Some(t) = transfers.get_mut(&(gid.to_owned(), fid.to_owned())) {
                        t.active = false;
                        t.error = Some(format!("{err:#}"));
                        t.failed_at = Some(Instant::now());
                    }
                }
            }
        }
        match result {
            Ok(path) => {
                // On Android this moves the file to the public Downloads folder.
                let path = tokio::task::spawn_blocking(move || crate::platform::file_received(&path)).await?;
                self.with_state(|st| st.received.push(Share { group: gid.to_owned(), file: fid.to_owned(), path: path.clone(), staged: false }));
                self.with_doc(gid, |d| d.mark_downloaded(fid, &me));
                self.emit(Event::Notice(format!("downloaded {}", offer.name)));
                self.doc_changed(gid, false);
                if self.0.store.lock().unwrap().state.groups.iter().any(|g| g.id == gid && g.auto_rehost) {
                    // Fails when the sender stopped sharing in the meantime; nothing to host then.
                    let _ = self.host_file(gid, fid);
                }
                Ok(path)
            }
            Err(err) => {
                self.emit(Event::Notice(format!("download of {} failed: {err:#}", offer.name)));
                self.publish_group(gid, false);
                Err(err)
            }
        }
    }

    async fn fetch(&self, gid: &str, fid: &str, offer: &crate::groupdoc::FileOffer) -> Result<PathBuf> {
        let me = self.me();
        let conn = std::iter::once(&offer.from)
            .chain(offer.hosts.iter().filter(|h| **h != offer.from && **h != me))
            .filter_map(|id| id.parse::<EndpointId>().ok())
            .find_map(|id| self.connection(id))
            .context("nobody who has it is online")?;
        let dir = self.download_dir();
        tokio::fs::create_dir_all(&dir).await.with_context(|| format!("cannot create {}", dir.display()))?;

        let (mut tx, mut rx) = conn.open_bi().await.map_err(|e| anyhow!("{e}"))?;
        let request = serde_json::to_vec(&DownloadRequest { group: gid.to_owned(), file: fid.to_owned() })?;
        tx.write_all(&request).await.map_err(|e| anyhow!("{e}"))?;
        tx.finish().map_err(|e| anyhow!("{e}"))?;

        let mut header = [0u8; 9];
        rx.read_exact(&mut header).await.map_err(|_| anyhow!("the sender refused or went away"))?;
        if header[0] != 0 {
            bail!("the sender no longer shares it");
        }
        let len = u64::from_be_bytes(header[1..].try_into().unwrap());
        if len != offer.size {
            bail!("the file changed on the sender's side");
        }

        let part = dir.join(format!(".{fid}.part"));
        let result = async {
            let mut out = tokio::fs::File::create(&part).await?;
            let mut hasher = blake3::Hasher::new();
            let mut buf = vec![0u8; CHUNK];
            let (mut done, mut shown) = (0u64, Instant::now());
            while let Some(n) = rx.read(&mut buf).await.map_err(|e| anyhow!("connection lost: {e}"))? {
                hasher.update(&buf[..n]);
                out.write_all(&buf[..n]).await?;
                done += n as u64;
                if let Some(t) = self.0.sharing.transfers.lock().unwrap().get_mut(&(gid.to_owned(), fid.to_owned())) {
                    t.done = done;
                }
                if shown.elapsed() > Duration::from_millis(200) {
                    shown = Instant::now();
                    self.publish_group(gid, false);
                }
            }
            out.flush().await?;
            if done != len {
                bail!("connection lost after {done} of {len} bytes");
            }
            if hasher.finalize().to_hex().as_str() != offer.hash {
                bail!("the file arrived corrupted");
            }
            Ok(())
        }
        .await;
        if let Err(err) = result {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(err);
        }
        let target = unique_path(&dir, &offer.name);
        tokio::fs::rename(&part, &target).await?;
        Ok(target)
    }

    /// The other side of [`Core::fetch`].
    pub(super) async fn serve_download(&self, from: EndpointId, mut tx: SendStream, mut rx: RecvStream) {
        if let Err(err) = self.serve(from, &mut tx, &mut rx).await {
            tracing::debug!("refused download to {}: {err:#}", from.fmt_short());
            let _ = tx.write_all(&[1u8; 9]).await;
        }
        let _ = tx.finish();
        let _ = tx.stopped().await;
    }

    async fn serve(&self, from: EndpointId, tx: &mut SendStream, rx: &mut RecvStream) -> Result<()> {
        let raw = rx.read_to_end(4096).await?;
        if raw == b"ping" {
            // Liveness probe (see `Core::drop_stale`).
            tx.write_all(&[0xFF]).await?;
            return Ok(());
        }
        let request: DownloadRequest = serde_json::from_slice(&raw)?;
        if !self.has_group(&request.group) || !self.group_peers(&request.group).contains(&from) {
            bail!("not a member");
        }
        let me = self.me();
        let offer = self
            .with_doc(&request.group, |d| d.file(&request.file))
            .filter(|f| (f.from == me || f.hosts.contains(&me)) && !f.stopped)
            .context("not on offer")?;
        let path = self
            .0
            .store
            .lock()
            .unwrap()
            .state
            .shares
            .iter()
            .find(|s| s.group == request.group && s.file == request.file)
            .map(|s| s.path.clone())
            .context("not shared here")?;
        let mut file = tokio::fs::File::from_std(tokio::task::spawn_blocking(move || open_source(&path)).await??);
        let len = file.metadata().await?.len();
        if len != offer.size {
            bail!("file changed since it was offered");
        }
        let mut header = vec![0u8];
        header.extend_from_slice(&len.to_be_bytes());
        tx.write_all(&header).await?;
        let mut buf = vec![0u8; CHUNK];
        loop {
            use tokio::io::AsyncReadExt;
            let n = file.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            tx.write_all(&buf[..n]).await?;
        }
        Ok(())
    }

    // ---- automatic behaviour ----------------------------------------------------------

    pub(super) fn auto_jobs_all(&self) {
        let groups: Vec<String> = self.0.store.lock().unwrap().state.groups.iter().map(|g| g.id.clone()).collect();
        for gid in groups {
            self.auto_jobs(&gid);
        }
    }

    /// Per-group local settings at work: download new offers, stop sharing what everybody has.
    pub(super) fn auto_jobs(&self, gid: &str) {
        let Some((auto_download, auto_stop)) =
            self.0.store.lock().unwrap().state.groups.iter().find(|g| g.id == gid).map(|g| (g.auto_download, g.auto_stop))
        else {
            return;
        };
        if !auto_download && !auto_stop {
            return;
        }
        let me = self.me();
        let (members, files) = self.with_doc(gid, |d| (d.members(), d.files()));
        for f in files.iter().filter(|f| !f.stopped) {
            if f.from == me {
                let audience: Vec<&String> = members.iter().map(|(id, _)| id).filter(|id| **id != me).collect();
                if auto_stop && !audience.is_empty() && audience.iter().all(|id| f.downloaded.contains_key(*id)) {
                    let _ = self.stop_file(gid, &f.id);
                }
            } else if auto_download && !f.downloaded.contains_key(&me) {
                let online = std::iter::once(&f.from).chain(&f.hosts).filter(|id| **id != me).any(|id| id.parse().ok().and_then(|id| self.connection(id)).is_some());
                if online {
                    let (this, gid, fid) = (self.clone(), gid.to_owned(), f.id.clone());
                    tokio::spawn(async move {
                        // Errors are shown on the file and retried later.
                        let _ = this.download(&gid, &fid, false).await;
                    });
                }
            }
        }
    }
}

/// Does the peer on this connection answer? A connection to a peer that restarted looks alive
/// until QUIC's idle timeout, so ask.
/// Open what a share points at: a file path, or on Android a `content://` URI the user picked.
pub(super) fn open_source(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(target_os = "android")]
    if path.to_str().is_some_and(|p| p.starts_with("content://")) {
        let fd = crate::platform::open_content_fd(&path.to_string_lossy()).ok_or_else(|| std::io::Error::other("the picked file is no longer readable"))?;
        return Ok(std::fs::File::from(fd));
    }
    std::fs::File::open(path)
}

impl Core {
    /// Shares that ended: delete our own copies, give back the permission to read picked files.
    pub(super) fn release_sources(&self, gone: Vec<Share>) {
        for s in gone {
            if s.staged {
                let _ = std::fs::remove_file(&s.path);
                continue;
            }
            #[cfg(target_os = "android")]
            if let Some(uri) = s.path.to_str().filter(|p| p.starts_with("content://")) {
                let still_used = self.0.store.lock().unwrap().state.shares.iter().any(|o| o.path == s.path);
                if !still_used {
                    crate::platform::release_content_uri(uri);
                }
            }
        }
    }
}

async fn probe(conn: &Connection) -> bool {
    let ask = async {
        let (mut tx, mut rx) = conn.open_bi().await.ok()?;
        tx.write_all(b"ping").await.ok()?;
        tx.finish().ok()?;
        rx.read_exact(&mut [0u8; 1]).await.ok()
    };
    tokio::time::timeout(Duration::from_secs(2), ask).await.is_ok_and(|r| r.is_some())
}

impl Core {
    /// An incoming connection is about to lose the duplicate tie-break against a connection we
    /// already hold. If that one is dead (the peer restarted), close it so the new one is kept.
    pub(super) async fn drop_stale(&self, incoming: &Connection) {
        let id = incoming.remote_id();
        if id < self.id() {
            return; // the incoming connection wins the tie-break anyway
        }
        let Some(existing) = self.connection(id) else { return };
        if !probe(&existing).await {
            tracing::debug!("dropping stale connection to {}", id.fmt_short());
            existing.close(0u32.into(), b"stale");
        }
    }
}

async fn send_bytes(conn: &Connection, bytes: &[u8]) -> Result<()> {
    let mut tx = conn.open_uni().await.map_err(|e| anyhow!("{e}"))?;
    tx.write_all(bytes).await.map_err(|e| anyhow!("{e}"))?;
    tx.finish().map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

/// `dir/name`, or `dir/name (2).ext` and so on if that is taken. The name is reduced to a bare file name.
pub(super) fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let name = clean_file_name(name);
    let first = dir.join(&name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.').filter(|i| *i > 0) {
        Some(i) => (&name[..i], &name[i..]),
        None => (name.as_str(), ""),
    };
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .expect("some name is free")
}

pub(super) fn clean_file_name(name: &str) -> String {
    let name = Path::new(name).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let name: String = name.chars().filter(|c| !c.is_control() && *c != '\\').collect();
    let name = name.trim();
    if name.is_empty() { "file".into() } else { name.to_owned() }
}
