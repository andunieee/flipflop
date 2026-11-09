//! What groups are for: files offered to the group, text snippets, a shared scratchpad. The shared
//! state is one Automerge document per group ([`GroupDoc`]), synced between its members over the
//! peer connections; file bytes travel straight from the offerer to whoever downloads them.
//!
//! Wire formats, next to the JSON control messages of [`super`]:
//! - sync: a uni stream holding `[SYNC_TAG, 16 raw group id bytes, automerge sync message]`;
//! - download: a bi stream; the downloader sends `{"group", "file"}` as JSON and the offerer answers
//!   `[status u8][length u64 BE][zstd stream of the bytes…]` (status 0 = ok, 1 = refused; the
//!   length is the uncompressed one). A folder's uncompressed bytes are
//!   `[listing length u32 BE][listing JSON][each file's bytes, in listing order]`, and its offer's
//!   hash is the blake3 of exactly those bytes (see [`Listing`]).

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    io::Write,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::{Context as TaskContext, Poll},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use async_compression::{
    Level,
    tokio::bufread::{ZstdDecoder, ZstdEncoder},
};
use iroh::{
    EndpointId,
    endpoint::{Connection, RecvStream, SendStream},
};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{
        AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader, BufWriter, ReadBuf,
        SeekFrom,
    },
    sync::Notify,
};

use super::{Core, Event, parse_gid, tabs::APP_KIND};
use crate::{
    groupdoc::GroupDoc,
    state::{MemberState, Share},
};

pub(super) const SYNC_TAG: u8 = 1;
/// What files move in. tokio does file I/O on another thread, so each read or write of one is a
/// round trip between threads: small ones cost much of a fast transfer.
const CHUNK: usize = 1024 * 1024;
/// An offer that is a whole folder.
pub const FOLDER_KIND: &str = "folder";
const MAX_LISTING_BYTES: usize = 64 << 20;
const MAX_TEXT_BYTES: usize = 256 * 1024;
const RETRY_AFTER: Duration = Duration::from_secs(15);
/// How often a job with its own progress (hashing, sending) updates the UI.
const SHOW_EVERY: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, PartialEq)]
pub struct DocMember {
    pub id: String,
    pub name: String,
    pub me: bool,
    pub online: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
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
    /// It can be offered (again): ours, stopped and still where it was; or someone else's, stopped,
    /// and our download still where it went.
    pub can_reoffer: bool,
    /// Ours: where we share it from (not for copies made for the offer).
    pub shared_from: Option<PathBuf>,
    /// It may leave our files tab: nothing we serve or are downloading.
    pub can_remove: bool,
    /// Someone who has it is online: the offerer or a host (we ourselves excluded).
    pub source_online: bool,
    /// We are still hashing it, so it is not on offer yet and `id` is only this card's; `progress`
    /// is how far along that is.
    pub preparing: bool,
    /// We called its download off: it is not fetched again until asked for.
    pub on_hold: bool,
    /// It can be put on hold: it is downloading, or would be automatically.
    pub can_hold: bool,
    /// Who is getting it from us right now.
    pub sending: Vec<SendView>,
}

/// One member getting a file from us.
#[derive(Debug, Clone, PartialEq)]
pub struct SendView {
    /// Their name.
    pub to: String,
    /// Bytes read from the file for them. Ahead of what they received by what is on its way.
    pub done: u64,
    pub total: u64,
}

/// One transfer, for the Home page's summary.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TransferItem {
    pub group: String,
    pub name: String,
    /// "download", "upload", "receive" (a guest's upload) or "prepare" (hashing an offer).
    pub kind: String,
    /// The member at the other end, when there is one.
    pub peer: String,
    pub done: u64,
    /// `None` while the size is not known.
    pub total: Option<u64>,
    /// Waiting for its turn: an automatic download that has not started yet.
    pub queued: bool,
}

/// Everything moving on this device right now, in every group.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TransfersNow {
    /// Downloads, and uploads from guests.
    pub receiving: usize,
    /// Copies going to members and guests.
    pub sending: usize,
    /// Offers being hashed.
    pub preparing: usize,
    /// Automatic downloads waiting for their turn (not counted in the three above).
    pub queued: usize,
    /// The file's name, when only one is moving.
    pub name: Option<String>,
    /// Bytes so far, of all of them.
    pub done: u64,
    /// `None` while the size of one of them is not known.
    pub total: Option<u64>,
    /// Every transfer and queued file, in the order they are shown on Home.
    pub items: Vec<TransferItem>,
}

impl TransfersNow {
    pub fn active(&self) -> bool {
        self.receiving + self.sending + self.preparing > 0
    }

    /// What to tell the user: a title, a detail, and the percentage done (-1 if not known).
    pub fn describe(&self) -> (String, String, i32) {
        if !self.active() {
            // Queued automatic downloads are not moving yet, but they are why the app must stay
            // awake: say so instead of leaving the notification empty (which stops the service).
            let title = match self.queued {
                0 => String::new(),
                1 => "Waiting to download 1 file".to_owned(),
                n => format!("Waiting to download {n} files"),
            };
            return (title, String::new(), -1);
        }
        let jobs = [
            ("downloading", self.receiving),
            ("sending", self.sending),
            ("preparing", self.preparing),
        ];
        let title = match &self.name {
            Some(name) => {
                let verb = jobs.iter().find(|j| j.1 > 0).map_or("moving", |j| j.0);
                format!("{verb} {name}")
            }
            None => jobs
                .iter()
                .filter(|j| j.1 > 0)
                .map(|(verb, n)| format!("{verb} {n} {}", if *n == 1 { "file" } else { "files" }))
                .collect::<Vec<_>>()
                .join(", "),
        };
        let mut chars = title.chars();
        let title = chars
            .next()
            .map(|c| c.to_uppercase().chain(chars).collect())
            .unwrap_or_default();
        let done = crate::format::size(self.done);
        match self.total {
            Some(total) if total > 0 => (
                title,
                format!("{done} of {}", crate::format::size(total)),
                (self.done as f64 / total as f64 * 100.0).min(100.0) as i32,
            ),
            _ if self.done > 0 => (title, done, -1),
            _ => (title, String::new(), -1),
        }
    }
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

#[derive(Debug, Clone, PartialEq)]
pub struct TabView {
    /// "files", "texts", "pad", or an app's id.
    pub id: String,
    pub title: String,
    /// The group shows it.
    pub on: bool,
    pub app: Option<AppView>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppView {
    /// Who added it.
    pub from_name: String,
    pub size: u64,
    /// We hold its wasm.
    pub ready: bool,
    /// Fetching it: (bytes so far, total).
    pub progress: Option<(u64, u64)>,
    pub error: Option<String>,
    /// Someone who has it is online (we ourselves excluded).
    pub source_online: bool,
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
    /// The built-in tabs, then the apps', shown or not.
    pub tabs: Vec<TabView>,
}

struct Transfer {
    group: String,
    name: String,
    /// Who offers it.
    from: String,
    done: u64,
    total: u64,
    active: bool,
    error: Option<String>,
    failed_at: Option<Instant>,
    /// Stops the download: it was put on hold.
    cancel: Arc<Notify>,
}

/// Bytes through a job, shared with the thread doing it.
#[derive(Default)]
struct Meter {
    done: AtomicU64,
    /// Zero until known.
    total: AtomicU64,
}

/// An offer being hashed: on the screen already, in the document once that is done.
struct Preparing {
    group: String,
    id: String,
    name: String,
    kind: String,
    path: PathBuf,
    ts: i64,
    meter: Arc<Meter>,
}

/// A file on its way to a member.
struct Sending {
    group: String,
    file: String,
    name: String,
    to: String,
    /// The member's name, for the Home page.
    to_name: String,
    total: u64,
    done: Arc<AtomicU64>,
}

/// An automatic download waiting for its turn.
struct Queued {
    name: String,
    size: u64,
    /// Who offers it.
    from: String,
    /// Its turn came. It is no longer shown as queued, even before its download shows.
    started: Arc<AtomicBool>,
}

/// A file a guest is uploading to us. How big it is will only be known at the end.
struct Incoming {
    group: String,
    name: String,
    done: Arc<AtomicU64>,
}

pub(super) struct Sharing {
    dir: PathBuf,
    /// Apps' wasm, named after its hash (see [`super::tabs`]).
    pub(super) apps_dir: PathBuf,
    docs: Mutex<HashMap<String, GroupDoc>>,
    dirty: Mutex<HashSet<String>>,
    transfers: Mutex<HashMap<(String, String), Transfer>>,
    /// Auto-downloads take turns: the files queued for one wait here.
    auto_turn: tokio::sync::Semaphore,
    /// What is queued for an auto-download, so a repeated job does not queue a file twice.
    auto_queued: Mutex<HashMap<(String, String), Queued>>,
    preparing: Mutex<Vec<Preparing>>,
    next_preparing: AtomicU64,
    sending: Mutex<Vec<Sending>>,
    incoming: Mutex<Vec<Incoming>>,
    /// What the UI was last told of what is moving.
    transfers_now: Mutex<TransfersNow>,
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
        let download_dir = dirs::download_dir()
            .unwrap_or_else(|| data_dir.join("downloads"))
            .join("flipflop");
        #[cfg(target_os = "android")]
        let download_dir = data_dir.join("downloads");
        Sharing {
            dir,
            apps_dir: data_dir.join("apps"),
            docs: Mutex::default(),
            dirty: Mutex::default(),
            transfers: Mutex::default(),
            auto_turn: tokio::sync::Semaphore::new(1),
            auto_queued: Mutex::default(),
            preparing: Mutex::default(),
            next_preparing: AtomicU64::new(0),
            sending: Mutex::default(),
            incoming: Mutex::default(),
            transfers_now: Mutex::default(),
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
    /// Start sending from this byte of the transfer's uncompressed stream.
    #[serde(default)]
    from: u64,
    /// The downloader understands the reply header with the start offset (see [`Core::serve`]).
    #[serde(default)]
    resume: bool,
}

impl Core {
    pub(super) fn me(&self) -> String {
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
        self.0
            .store
            .lock()
            .unwrap()
            .state
            .groups
            .iter()
            .any(|g| g.id == gid)
    }

    pub(super) fn require_group(&self, gid: &str) -> Result<()> {
        self.has_group(gid)
            .then_some(())
            .ok_or_else(|| anyhow!("no such group"))
    }

    // ---- documents --------------------------------------------------------------------

    /// Run `f` on the group's document, loading it from disk (or starting it) first.
    pub(super) fn with_doc<R>(&self, gid: &str, f: impl FnOnce(&mut GroupDoc) -> R) -> R {
        let sharing = &self.0.sharing;
        let mut docs = sharing.docs.lock().unwrap();
        let doc = docs.entry(gid.to_owned()).or_insert_with(|| {
            std::fs::read(sharing.doc_path(gid))
                .ok()
                .and_then(|b| {
                    GroupDoc::load(&b)
                        .map_err(|e| tracing::warn!("group document unreadable: {e}"))
                        .ok()
                })
                .unwrap_or_else(GroupDoc::new)
        });
        f(doc)
    }

    /// Load every group's document, announce ourselves in it and keep it saved.
    pub(super) fn load_docs(&self) {
        let groups: Vec<String> = self
            .0
            .store
            .lock()
            .unwrap()
            .state
            .groups
            .iter()
            .map(|g| g.id.clone())
            .collect();
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
            if let Err(err) = std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, &path))
            {
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
        let groups: Vec<String> = self
            .0
            .sharing
            .docs
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect();
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
            let (gone, keep) = std::mem::take(&mut st.shares)
                .into_iter()
                .partition(|s| s.group == gid);
            st.shares = keep;
            st.stopped_shares.retain(|s| s.group != gid);
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
                .flat_map(|g| {
                    g.members
                        .iter()
                        .filter(|m| m.state == MemberState::Accepted)
                        .map(|m| m.peer.clone())
                })
                .collect()
        };
        ids.extend(
            self.with_doc(gid, |d| d.members())
                .into_iter()
                .map(|(id, _)| id),
        );
        let me = self.id();
        ids.iter()
            .filter_map(|id| id.parse().ok())
            .filter(|id| *id != me)
            .collect()
    }

    /// Everyone any of our groups' documents lists as a member.
    pub(super) fn doc_peers(&self) -> Vec<EndpointId> {
        let me = self.id();
        let docs = self.0.sharing.docs.lock().unwrap();
        let ids: BTreeSet<String> = docs
            .values()
            .flat_map(|d| d.members())
            .map(|(id, _)| id)
            .collect();
        ids.iter()
            .filter_map(|id| id.parse().ok())
            .filter(|id| *id != me)
            .collect()
    }

    pub(super) fn connection(&self, peer: EndpointId) -> Option<Connection> {
        self.0
            .peers
            .lock()
            .unwrap()
            .get(&peer)
            .and_then(|p| p.conn.clone())
            .filter(|c| c.close_reason().is_none())
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
        let (members, files, texts, scratch, tabs) = self.with_doc(gid, |d| {
            (d.members(), d.files(), d.texts(), d.scratch(), d.tabs())
        });
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
        // Where we hold a copy: what we downloaded, and what we share, as long as it is still there.
        let received: HashMap<String, PathBuf> = {
            let store = self.0.store.lock().unwrap();
            let state = &store.state;
            state
                .shares
                .iter()
                .chain(&state.received)
                .filter(|r| r.group == gid && r.path.exists())
                .map(|r| (r.file.clone(), r.path.clone()))
                .collect()
        };
        // Read-only: `build_view` runs while the publish lock is held (and from `with_state`), so it
        // must not go through `with_state`, which publishes again.
        let reofferable: HashSet<String> = {
            let store = self.0.store.lock().unwrap();
            store
                .state
                .stopped_shares
                .iter()
                .filter(|s| s.group == gid && s.path.exists())
                .map(|s| s.file.clone())
                .collect()
        };
        // Where our own files are (shared or stopped), and what we downloaded; the files we removed from the tab.
        let (sources, downloads, hidden): (
            HashMap<String, PathBuf>,
            HashMap<String, PathBuf>,
            HashSet<String>,
        ) = {
            let store = self.0.store.lock().unwrap();
            let state = &store.state;
            let here = |s: &&Share| s.group == gid && s.path.exists();
            (
                state
                    .shares
                    .iter()
                    .filter(|s| !s.staged)
                    .chain(&state.stopped_shares)
                    .filter(here)
                    .map(|s| (s.file.clone(), s.path.clone()))
                    .collect(),
                state
                    .received
                    .iter()
                    .filter(here)
                    .map(|s| (s.file.clone(), s.path.clone()))
                    .collect(),
                state
                    .groups
                    .iter()
                    .find(|g| g.id == gid)
                    .map(|g| g.hidden_files.iter().cloned().collect())
                    .unwrap_or_default(),
            )
        };
        // What we put on hold, and whether what is not on hold comes by itself.
        let (held, auto_download): (HashSet<String>, bool) = {
            let store = self.0.store.lock().unwrap();
            store
                .state
                .groups
                .iter()
                .find(|g| g.id == gid)
                .map(|g| (g.held_files.iter().cloned().collect(), g.auto_download))
                .unwrap_or_default()
        };
        // Hashes we offer right now, so a file we already shared again is not offered twice.
        let offering: HashSet<&str> = files
            .iter()
            .filter(|f| f.from == me && !f.stopped)
            .map(|f| f.hash.as_str())
            .collect();
        let transfers = self.0.sharing.transfers.lock().unwrap();
        let sending = self.0.sharing.sending.lock().unwrap();
        let name_of = |id: &str| {
            members
                .iter()
                .find(|(m, _)| m == id)
                .map_or_else(|| id.chars().take(8).collect(), |(_, n)| n.clone())
        };

        // Old offers nobody shares anymore and we hold no copy of are not worth listing.
        let week_ms = 7 * 24 * 3600 * 1000;
        let now = crate::groupdoc::now_ms();
        let mut file_views: Vec<FileView> = files
            .iter()
            .filter(|f| f.kind != APP_KIND && !hidden.contains(&f.id))
            .filter(|f| {
                let since = if f.stopped_at > 0 { f.stopped_at } else { f.ts };
                !(f.stopped && !received.contains_key(&f.id) && now - since > week_ms)
            })
            .map(|f| {
                let mine = f.from == me;
                let t = transfers.get(&(gid.to_owned(), f.id.clone()));
                let audience = members.iter().filter(|(id, _)| *id != f.from).count();
                let have = mine || f.downloaded.contains_key(&me);
                let on_hold = held.contains(&f.id) && !have;
                let downloading = t.is_some_and(|t| t.active);
                FileView {
                    id: f.id.clone(),
                    name: f.name.clone(),
                    size: f.size,
                    kind: f.kind.clone(),
                    from: f.from.clone(),
                    from_name: if f.from_name.is_empty() {
                        name_of(&f.from)
                    } else {
                        f.from_name.clone()
                    },
                    mine,
                    ts: f.ts,
                    stopped: f.stopped,
                    downloaded_by: f
                        .downloaded
                        .keys()
                        .filter(|id| **id != f.from)
                        .map(|id| name_of(id))
                        .collect(),
                    audience,
                    have,
                    saved_to: received.get(&f.id).cloned(),
                    progress: t.filter(|t| t.active).map(|t| (t.done, t.total)),
                    error: t.and_then(|t| t.error.clone()),
                    offerer_online: online.contains(&f.from),
                    hosts: f
                        .hosts
                        .iter()
                        .filter(|id| **id != f.from)
                        .map(|id| name_of(id))
                        .collect(),
                    hosting: f.hosts.contains(&me),
                    can_reoffer: f.stopped
                        && if mine {
                            reofferable.contains(&f.id)
                                && !files.iter().any(|o| {
                                    o.from == me
                                        && !o.stopped
                                        && o.hash == f.hash
                                        && o.name == f.name
                                })
                        } else {
                            downloads.contains_key(&f.id) && !offering.contains(f.hash.as_str())
                        },
                    shared_from: mine.then(|| sources.get(&f.id).cloned()).flatten(),
                    can_remove: (f.stopped || !mine && !f.hosts.contains(&me)) && !downloading,
                    source_online: online.contains(&f.from)
                        || f.hosts.iter().any(|h| *h != me && online.contains(h)),
                    on_hold,
                    can_hold: !have && !f.stopped && !on_hold && (downloading || auto_download),
                    sending: sending
                        .iter()
                        .filter(|s| s.group == gid && s.file == f.id)
                        .map(|s| SendView {
                            to: name_of(&s.to),
                            done: s.done.load(Ordering::Relaxed),
                            total: s.total,
                        })
                        .collect(),
                    ..FileView::default()
                }
            })
            .collect();
        file_views.reverse();
        // Not in the document yet, and the newest of all.
        let preparing: Vec<FileView> = self
            .0
            .sharing
            .preparing
            .lock()
            .unwrap()
            .iter()
            .rev()
            .filter(|p| p.group == gid)
            .map(|p| {
                let total = p.meter.total.load(Ordering::Relaxed);
                FileView {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    size: total,
                    kind: p.kind.clone(),
                    from: me.clone(),
                    mine: true,
                    ts: p.ts,
                    have: true,
                    progress: Some((p.meter.done.load(Ordering::Relaxed), total)),
                    preparing: true,
                    shared_from: Some(p.path.clone()),
                    ..FileView::default()
                }
            })
            .collect();
        file_views.splice(0..0, preparing);
        let tab_views = tabs
            .into_iter()
            .map(|tab| {
                let Some(app) = tab.app else {
                    let title = match tab.id.as_str() {
                        "files" => "Files",
                        "texts" => "Texts",
                        _ => "Scratchpad",
                    };
                    return TabView {
                        title: title.to_owned(),
                        id: tab.id,
                        on: tab.on,
                        app: None,
                    };
                };
                let t = transfers.get(&(gid.to_owned(), app.file.clone()));
                let offer = files.iter().find(|f| f.id == app.file);
                TabView {
                    title: app
                        .name
                        .strip_suffix(".wasm")
                        .unwrap_or(&app.name)
                        .to_owned(),
                    id: tab.id,
                    on: tab.on,
                    app: Some(AppView {
                        from_name: name_of(&app.from),
                        size: app.size,
                        ready: self.app_path(&app.hash).exists(),
                        progress: t.filter(|t| t.active).map(|t| (t.done, t.total)),
                        error: t.and_then(|t| t.error.clone()),
                        source_online: offer.is_some_and(|f| {
                            !f.stopped
                                && std::iter::once(&f.from)
                                    .chain(&f.hosts)
                                    .any(|h| *h != me && online.contains(h))
                        }),
                    }),
                }
            })
            .collect();
        let mut text_views: Vec<TextView> = texts
            .into_iter()
            .map(|t| TextView {
                mine: t.from == me,
                id: t.id,
                from: t.from,
                name: t.name,
                text: t.text,
                ts: t.ts,
            })
            .collect();
        text_views.reverse();
        GroupView {
            id: gid.to_owned(),
            members: members
                .iter()
                .map(|(id, name)| DocMember {
                    id: id.clone(),
                    name: name.clone(),
                    me: *id == me,
                    online: online.contains(id),
                })
                .collect(),
            files: file_views,
            texts: text_views,
            scratch,
            scratch_local,
            tabs: tab_views,
        }
    }

    pub(super) fn publish_group(&self, gid: &str, scratch_local: bool) {
        let _ordered = self.0.publish_lock.lock().unwrap();
        self.emit_group(gid, scratch_local, true);
        self.emit_home();
        self.emit_transfers();
    }

    /// Tell the UI the queue changed. The caller does not hold the publish lock.
    fn publish_transfers(&self) {
        let _ordered = self.0.publish_lock.lock().unwrap();
        self.emit_transfers();
    }

    /// Everything moving on this device right now, and what waits for its turn.
    pub fn transfers_now(&self) -> TransfersNow {
        let sharing = &self.0.sharing;
        let mut now = TransfersNow {
            total: Some(0),
            ..TransfersNow::default()
        };
        let mut names = Vec::new();
        let mut add = |now: &mut TransfersNow, name: &str, done: u64, total: Option<u64>| {
            names.push(name.to_owned());
            now.done += done;
            now.total = now.total.zip(total).map(|(a, b)| a + b);
        };
        {
            let transfers = sharing.transfers.lock().unwrap();
            for t in transfers.values() {
                if t.active {
                    now.receiving += 1;
                    add(&mut now, &t.name, t.done, Some(t.total));
                    now.items.push(TransferItem {
                        group: t.group.clone(),
                        name: t.name.clone(),
                        kind: "download".into(),
                        peer: t.from.clone(),
                        done: t.done,
                        total: Some(t.total),
                        queued: false,
                    });
                }
            }
            for ((gid, fid), q) in sharing.auto_queued.lock().unwrap().iter() {
                // The one that has its turn is described above as a download.
                if q.started.load(Ordering::Relaxed)
                    || transfers.contains_key(&(gid.clone(), fid.clone()))
                {
                    continue;
                }
                now.queued += 1;
                now.items.push(TransferItem {
                    group: gid.clone(),
                    name: q.name.clone(),
                    kind: "download".into(),
                    peer: q.from.clone(),
                    done: 0,
                    total: Some(q.size),
                    queued: true,
                });
            }
        }
        for up in sharing.incoming.lock().unwrap().iter() {
            now.receiving += 1;
            let done = up.done.load(Ordering::Relaxed);
            add(&mut now, &up.name, done, None);
            now.items.push(TransferItem {
                group: up.group.clone(),
                name: up.name.clone(),
                kind: "receive".into(),
                done,
                ..TransferItem::default()
            });
        }
        for s in sharing.sending.lock().unwrap().iter() {
            now.sending += 1;
            let done = s.done.load(Ordering::Relaxed);
            add(&mut now, &s.name, done, Some(s.total));
            now.items.push(TransferItem {
                group: s.group.clone(),
                name: s.name.clone(),
                kind: "upload".into(),
                peer: s.to_name.clone(),
                done,
                total: Some(s.total),
                queued: false,
            });
        }
        for p in sharing.preparing.lock().unwrap().iter() {
            now.preparing += 1;
            let total = p.meter.total.load(Ordering::Relaxed);
            let done = p.meter.done.load(Ordering::Relaxed);
            add(&mut now, &p.name, done, (total > 0).then_some(total));
            now.items.push(TransferItem {
                group: p.group.clone(),
                name: p.name.clone(),
                kind: "prepare".into(),
                done,
                total: (total > 0).then_some(total),
                ..TransferItem::default()
            });
        }
        if let [name] = names.as_slice() {
            now.name = Some(name.clone());
        }
        if !now.active() && now.queued == 0 {
            return TransfersNow::default();
        }
        now
    }

    /// Tell the UI what is moving, if that changed. The caller holds the publish lock.
    fn emit_transfers(&self) {
        let now = self.transfers_now();
        let mut shown = self.0.sharing.transfers_now.lock().unwrap();
        if *shown == now {
            return;
        }
        *shown = now.clone();
        drop(shown);
        self.emit(Event::Transfers(now));
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
        let ids: Vec<String> = self
            .0
            .store
            .lock()
            .unwrap()
            .state
            .groups
            .iter()
            .map(|g| g.id.clone())
            .collect();
        for gid in ids {
            self.emit_group(&gid, false, false);
        }
    }

    /// Re-send every group's view (the UI just started).
    pub fn publish_groups(&self) {
        let _ordered = self.0.publish_lock.lock().unwrap();
        let ids: Vec<String> = self
            .0
            .store
            .lock()
            .unwrap()
            .state
            .groups
            .iter()
            .map(|g| g.id.clone())
            .collect();
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
        let groups: Vec<String> = self
            .0
            .store
            .lock()
            .unwrap()
            .state
            .groups
            .iter()
            .map(|g| g.id.clone())
            .collect();
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
            let Some(conn) = self.connection(peer) else {
                return;
            };
            let Some(message) = self.with_doc(gid, |d| d.sync_message(&key)) else {
                break;
            };
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
        let mine = self.with_doc(gid, |d| {
            d.texts().iter().any(|t| t.id == text_id && t.from == me)
        });
        if !mine {
            bail!("not your snippet");
        }
        self.with_doc(gid, |d| d.delete_text(text_id));
        self.doc_changed(gid, false);
        Ok(())
    }

    pub fn scratch(&self, gid: &str) -> Option<String> {
        self.has_group(gid)
            .then(|| self.with_doc(gid, |d| d.scratch()))
    }

    /// Replace the scratchpad text; only the differing span is edited, so concurrent edits merge.
    pub fn set_scratch(&self, gid: &str, text: &str) {
        if !self.has_group(gid) {
            return;
        }
        if self.with_doc(gid, |d| d.set_scratch(text)) {
            let now = self.with_doc(gid, |d| d.scratch());
            self.0
                .sharing
                .own_pad
                .lock()
                .unwrap()
                .insert(gid.to_owned(), super::unseen::hash(&now));
            self.doc_changed(gid, true);
        }
    }

    // ---- files ------------------------------------------------------------------------

    /// Offer a file to the group, under `name` (default: the file's own). `staged`: the file at `path` is our own copy (made for the offer),
    /// removed again when sharing stops. `kind` is "file", "media" or "apk".
    pub async fn offer_file(
        &self,
        gid: &str,
        path: PathBuf,
        name: Option<String>,
        kind: &str,
        staged: bool,
    ) -> Result<String> {
        self.require_group(gid)?;
        let name = match name {
            Some(name) => clean_file_name(&name),
            None => path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .context("that path has no file name")?,
        };
        let result = self.add_offer(gid, &path, &name, kind, staged).await;
        if result.is_err() {
            // Nothing will serve it: our copy and the permission to read it can go.
            self.release_sources(vec![Share {
                group: gid.to_owned(),
                file: String::new(),
                path,
                staged,
            }]);
        }
        result
    }

    /// Hash what is at `path`, showing it in the meantime (that can take long), and put it in the
    /// group's document, unless the group has the same file (the same name and bytes) on offer.
    async fn add_offer(
        &self,
        gid: &str,
        path: &Path,
        name: &str,
        kind: &str,
        staged: bool,
    ) -> Result<String> {
        let kind = if path.is_dir() { FOLDER_KIND } else { kind };
        // An app is not listed with the files, and adding it twice makes two tabs.
        let app = kind == APP_KIND;
        let meter = Arc::new(Meter::default());
        let shown = if app {
            None
        } else {
            Some(self.begin_preparing(gid, path, name, kind, meter.clone())?)
        };
        let (hash, size) = hash_source(path.to_owned(), meter).await?;
        let (me, my_name) = (self.me(), self.name());
        let online: HashSet<String> = self
            .0
            .peers
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, p)| p.conn.as_ref().is_some_and(|c| c.close_reason().is_none()))
            .map(|(id, _)| id.to_string())
            .collect();
        let added = self.with_doc(gid, |d| {
            // Someone else's offer only counts if it can be downloaded now.
            let holder = d.files().into_iter().find(|f| {
                !app && f.kind != APP_KIND
                    && !f.stopped
                    && f.name == name
                    && f.hash == hash
                    && (f.from == me
                        || f.hosts.contains(&me)
                        || std::iter::once(&f.from)
                            .chain(&f.hosts)
                            .any(|id| online.contains(id)))
            });
            match holder {
                Some(f) if f.from == me => Err("you already share".to_owned()),
                Some(f) => Err(format!("{} already shares", f.from_name)),
                None => Ok(d.offer_file(&me, &my_name, name, size, &hash, kind)),
            }
        });
        let fid = added.map_err(|who| anyhow!("{who} {name}"))?;
        drop(shown);
        self.with_state(|st| {
            st.shares.push(Share {
                group: gid.to_owned(),
                file: fid.clone(),
                path: path.to_owned(),
                staged,
            })
        });
        self.doc_changed(gid, false);
        Ok(fid)
    }

    /// Show an offer being hashed until the returned guard goes.
    fn begin_preparing(
        &self,
        gid: &str,
        path: &Path,
        name: &str,
        kind: &str,
        meter: Arc<Meter>,
    ) -> Result<PreparingGuard> {
        let id = {
            let mut preparing = self.0.sharing.preparing.lock().unwrap();
            if preparing.iter().any(|p| p.group == gid && p.path == path) {
                bail!("{name} is already being added");
            }
            let n = self
                .0
                .sharing
                .next_preparing
                .fetch_add(1, Ordering::Relaxed);
            let id = format!("preparing-{n}");
            preparing.push(Preparing {
                group: gid.to_owned(),
                id: id.clone(),
                name: name.to_owned(),
                kind: kind.to_owned(),
                path: path.to_owned(),
                ts: crate::groupdoc::now_ms(),
                meter,
            });
            id
        };
        self.publish_group(gid, false);
        Ok(PreparingGuard {
            core: self.clone(),
            group: gid.to_owned(),
            id,
            _ticker: self.ticker(gid),
        })
    }

    /// Keep the group's view current while something it shows moves, until the ticker goes.
    fn ticker(&self, gid: &str) -> Ticker {
        let (this, gid) = (self.clone(), gid.to_owned());
        Ticker(
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(SHOW_EVERY).await;
                    let _ordered = this.0.publish_lock.lock().unwrap();
                    this.emit_group(&gid, false, false);
                    this.emit_transfers();
                }
            })
            .abort_handle(),
        )
    }

    /// Show a file going to `to` (a member's id) until the returned meter goes.
    pub(super) fn track_send(
        &self,
        gid: &str,
        fid: &str,
        name: &str,
        to: &str,
        to_name: &str,
        total: u64,
    ) -> SendMeter {
        let done = Arc::new(AtomicU64::new(0));
        self.0.sharing.sending.lock().unwrap().push(Sending {
            group: gid.to_owned(),
            file: fid.to_owned(),
            name: name.to_owned(),
            to: to.to_owned(),
            to_name: to_name.to_owned(),
            total,
            done: done.clone(),
        });
        self.publish_group(gid, false);
        SendMeter {
            core: self.clone(),
            group: gid.to_owned(),
            done,
            _ticker: self.ticker(gid),
        }
    }

    /// A guest uploaded a file (now at `path`): offer it to the group in their name, served from here.
    pub(super) fn add_guest_offer(
        &self,
        gid: &str,
        guest: &str,
        guest_name: &str,
        path: PathBuf,
        name: &str,
        size: u64,
        hash: &str,
    ) -> String {
        let me = self.me();
        let fid = self.with_doc(gid, |d| {
            let fid = d.offer_file(guest, guest_name, name, size, hash, "file");
            d.set_hosting(&fid, &me, true);
            d.mark_downloaded(&fid, &me);
            fid
        });
        self.with_state(|st| {
            st.shares.push(Share {
                group: gid.to_owned(),
                file: fid.clone(),
                path,
                staged: false,
            })
        });
        self.doc_changed(gid, false);
        fid
    }

    /// Stop offering one of our files; nobody can download it afterwards.
    pub fn stop_file(&self, gid: &str, file: &str) -> Result<()> {
        self.require_group(gid)?;
        let me = self.me();
        // A guest's upload is offered in their name, but we are the one serving it.
        let Some(mine) = self.with_doc(gid, |d| d.file(file)).and_then(|f| {
            if f.from == me {
                Some(true)
            } else {
                (super::guest::is_guest(&f.from) && f.hosts.contains(&me)).then_some(false)
            }
        }) else {
            bail!("you are not sharing that file");
        };
        self.with_doc(gid, |d| d.stop_file(file));
        let gone = self.with_state(|st| {
            let (gone, keep) = std::mem::take(&mut st.shares)
                .into_iter()
                .partition::<Vec<_>, _>(|s| s.group == gid && s.file == file);
            st.shares = keep;
            if mine {
                st.stopped_shares
                    .extend(gone.iter().filter(|s| !s.staged).cloned());
            }
            gone
        });
        self.release_sources(gone);
        self.doc_changed(gid, false);
        Ok(())
    }

    /// Count a guest's upload as moving until the returned meter goes.
    pub(super) fn track_incoming(&self, gid: &str, name: &str) -> IncomingMeter {
        let done = Arc::new(AtomicU64::new(0));
        self.0.sharing.incoming.lock().unwrap().push(Incoming {
            group: gid.to_owned(),
            name: name.to_owned(),
            done: done.clone(),
        });
        self.publish_group(gid, false);
        IncomingMeter {
            core: self.clone(),
            group: gid.to_owned(),
            done,
            _ticker: self.ticker(gid),
        }
    }

    /// Offer a stopped file again, if it is still where it was. Ours unchanged, the same entry
    /// comes back; changed, or someone else's we downloaded, it is offered anew, by us. Returns the file's id.
    pub async fn reoffer_file(&self, gid: &str, fid: &str) -> Result<String> {
        self.require_group(gid)?;
        let me = self.me();
        let offer = self
            .with_doc(gid, |d| d.file(fid))
            .context("no such file")?;
        if !offer.stopped {
            bail!("it is still shared");
        }
        if offer.from != me {
            if self
                .with_doc(gid, |d| d.files())
                .iter()
                .any(|f| f.from == me && !f.stopped && f.hash == offer.hash)
            {
                bail!("you share it already");
            }
            let path = self
                .with_state(|st| {
                    st.received
                        .iter()
                        .find(|r| r.group == gid && r.file == fid)
                        .map(|r| r.path.clone())
                })
                .context("you did not download it")?;
            if !path.exists() {
                bail!("{} is not there anymore", path.display());
            }
            return self
                .offer_file(gid, path, Some(offer.name), &offer.kind, false)
                .await;
        }
        let share = self
            .with_state(|st| {
                st.stopped_shares
                    .iter()
                    .find(|s| s.group == gid && s.file == fid)
                    .cloned()
            })
            .context("where that file was is not known anymore")?;
        if !share.path.exists() {
            bail!("{} is not there anymore", share.path.display());
        }
        let (hash, size) = hash_source(share.path.clone(), Arc::default()).await?;
        let forget_stopped = |st: &mut crate::state::State| {
            st.stopped_shares
                .retain(|s| !(s.group == gid && s.file == fid))
        };
        if hash != offer.hash || size != offer.size {
            let new = self
                .offer_file(gid, share.path, Some(offer.name), &offer.kind, false)
                .await?;
            self.with_state(forget_stopped);
            return Ok(new);
        }
        if self
            .with_doc(gid, |d| d.files())
            .iter()
            .any(|f| f.from == me && !f.stopped && f.hash == offer.hash && f.name == offer.name)
        {
            bail!("you already share {}", offer.name);
        }
        self.with_state(forget_stopped);
        self.with_doc(gid, |d| d.unstop_file(fid));
        self.with_state(|st| st.shares.push(share));
        self.doc_changed(gid, false);
        Ok(fid.to_owned())
    }

    /// Take a file off our files tab (the others keep it). Not one we serve or are downloading;
    /// a file of ours can then not be offered again. The downloaded copy stays where it is.
    pub fn remove_file(&self, gid: &str, fid: &str) -> Result<()> {
        self.require_group(gid)?;
        let me = self.me();
        let offer = self
            .with_doc(gid, |d| d.file(fid))
            .context("no such file")?;
        if !offer.stopped && offer.from == me {
            bail!("stop sharing it first");
        }
        if !offer.stopped && offer.hosts.contains(&me) {
            bail!("stop hosting it first");
        }
        if self
            .0
            .sharing
            .transfers
            .lock()
            .unwrap()
            .get(&(gid.to_owned(), fid.to_owned()))
            .is_some_and(|t| t.active)
        {
            bail!("it is still downloading");
        }
        self.with_state(|st| {
            st.stopped_shares
                .retain(|s| !(s.group == gid && s.file == fid));
            if let Some(g) = st.groups.iter_mut().find(|g| g.id == gid) {
                g.held_files.retain(|h| h != fid);
                if !g.hidden_files.iter().any(|h| h == fid) {
                    g.hidden_files.push(fid.to_owned());
                }
            }
        });
        Ok(())
    }

    /// Call off a file's download, if it is running, and don't fetch it again until
    /// [`Core::download_file`] is called for it, whatever the group's settings. What arrived so
    /// far is thrown away. The others are not told: to them, we just don't have it yet.
    pub fn hold_file(&self, gid: &str, fid: &str) -> Result<()> {
        self.require_group(gid)?;
        let me = self.me();
        let offer = self
            .with_doc(gid, |d| d.file(fid))
            .context("no such file")?;
        if offer.from == me {
            bail!("that is your own file");
        }
        if offer.downloaded.contains_key(&me) {
            bail!("already downloaded");
        }
        self.with_state(|st| {
            if let Some(g) = st.groups.iter_mut().find(|g| g.id == gid)
                && !g.held_files.iter().any(|h| h == fid)
            {
                g.held_files.push(fid.to_owned());
            }
        });
        {
            let mut transfers = self.0.sharing.transfers.lock().unwrap();
            let key = (gid.to_owned(), fid.to_owned());
            match transfers.get(&key) {
                Some(t) if t.active => t.cancel.notify_one(),
                // A failed attempt: its error is old news.
                Some(_) => {
                    transfers.remove(&key);
                }
                None => {}
            }
        }
        self.publish_group(gid, false);
        Ok(())
    }

    /// Serve a file we downloaded to the rest of the group too, so it stays available when the
    /// offerer is offline. Hosting ends when the offerer stops sharing, or with [`Core::unhost_file`].
    pub fn host_file(&self, gid: &str, fid: &str) -> Result<()> {
        self.require_group(gid)?;
        let me = self.me();
        let offer = self
            .with_doc(gid, |d| d.file(fid))
            .context("no such file")?;
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
                st.shares.push(Share {
                    group: gid.to_owned(),
                    file: fid.to_owned(),
                    path,
                    staged: false,
                });
            }
        });
        self.with_doc(gid, |d| d.set_hosting(fid, &me, true));
        self.doc_changed(gid, false);
        Ok(())
    }

    pub fn unhost_file(&self, gid: &str, fid: &str) -> Result<()> {
        self.require_group(gid)?;
        let me = self.me();
        if self
            .with_doc(gid, |d| d.file(fid))
            .is_some_and(|f| f.from == me)
        {
            bail!("that is your own file");
        }
        self.with_state(|st| st.shares.retain(|s| !(s.group == gid && s.file == fid)));
        self.with_doc(gid, |d| d.set_hosting(fid, &me, false));
        self.doc_changed(gid, false);
        Ok(())
    }

    /// `None` if it is already running, or failed too recently to retry by itself.
    fn begin_transfer(
        &self,
        gid: &str,
        fid: &str,
        name: &str,
        from: &str,
        total: u64,
        manual: bool,
    ) -> Option<Arc<Notify>> {
        let mut transfers = self.0.sharing.transfers.lock().unwrap();
        let key = (gid.to_owned(), fid.to_owned());
        if let Some(t) = transfers.get(&key) {
            let retry_ok = manual || t.failed_at.is_none_or(|at| at.elapsed() >= RETRY_AFTER);
            if t.active || !retry_ok {
                return None;
            }
        }
        let cancel = Arc::new(Notify::new());
        transfers.insert(
            key,
            Transfer {
                group: gid.to_owned(),
                name: name.to_owned(),
                from: from.to_owned(),
                done: 0,
                total,
                active: true,
                error: None,
                failed_at: None,
                cancel: cancel.clone(),
            },
        );
        Some(cancel)
    }

    /// Download an offered file into [`Core::download_dir`], taking it off hold; returns where it
    /// went. Resolves when done. Progress and errors show up in the group's [`FileView`].
    pub async fn download_file(&self, gid: &str, fid: &str) -> Result<PathBuf> {
        self.require_group(gid)?;
        self.with_state(|st| {
            if let Some(g) = st.groups.iter_mut().find(|g| g.id == gid) {
                g.held_files.retain(|h| h != fid);
            }
        });
        self.download(gid, fid, true).await
    }

    pub(super) async fn download(&self, gid: &str, fid: &str, manual: bool) -> Result<PathBuf> {
        self.require_group(gid)?;
        let offer = self
            .with_doc(gid, |d| d.file(fid))
            .context("no such file")?;
        let me = self.me();
        if offer.from == me {
            bail!("that is your own file");
        }
        if offer.stopped {
            bail!("the sender stopped sharing it");
        }
        if offer.downloaded.contains_key(&me)
            && self
                .0
                .store
                .lock()
                .unwrap()
                .state
                .received
                .iter()
                .any(|r| r.file == fid && r.path.exists())
        {
            bail!("already downloaded");
        }
        // The hold may have come after the automatic jobs decided to fetch it.
        if !manual
            && self
                .0
                .store
                .lock()
                .unwrap()
                .state
                .groups
                .iter()
                .any(|g| g.id == gid && g.held_files.iter().any(|h| h == fid))
        {
            bail!("{} is on hold", offer.name);
        }
        let from = if offer.from_name.is_empty() {
            &offer.from
        } else {
            &offer.from_name
        };
        let Some(cancel) = self.begin_transfer(gid, fid, &offer.name, from, offer.size, manual)
        else {
            bail!("already downloading");
        };
        self.publish_group(gid, false);
        // An app's wasm goes with the apps, named after its hash; anything else to the downloads.
        let target = (offer.kind == APP_KIND).then(|| self.app_path(&offer.hash));
        // Dropping the fetch drops its streams, which tells the sender to stop.
        let fetched = tokio::select! {
            result = self.fetch(gid, fid, &offer, target) => Some(result),
            _ = cancel.notified() => None,
        };
        let Some(result) = fetched else {
            let part = part_path(&self.download_dir(), fid);
            // The part is a plain file now; older versions kept a folder's as a directory.
            let _ = tokio::fs::remove_file(&part).await;
            let _ = tokio::fs::remove_dir_all(&part).await;
            self.end_transfer(gid, fid);
            self.publish_group(gid, false);
            bail!("{} is on hold", offer.name);
        };
        {
            let mut transfers = self.0.sharing.transfers.lock().unwrap();
            // Done, it is still moving until it is where it belongs (below).
            if let Err(err) = &result
                && let Some(t) = transfers.get_mut(&(gid.to_owned(), fid.to_owned()))
            {
                t.active = false;
                t.error = Some(format!("{err:#}"));
                t.failed_at = Some(Instant::now());
            }
        }
        match result {
            Ok(path) if offer.kind == APP_KIND => {
                self.end_transfer(gid, fid);
                self.serve_app(gid, fid, path.clone());
                Ok(path)
            }
            Ok(path) => {
                // On Android this moves the file to the public Downloads folder (not folders: they
                // stay in the app's download folder).
                let path = if offer.kind == FOLDER_KIND {
                    path
                } else {
                    match tokio::task::spawn_blocking(move || crate::platform::file_received(&path))
                        .await
                    {
                        Ok(path) => path,
                        Err(err) => {
                            self.end_transfer(gid, fid);
                            self.publish_group(gid, false);
                            return Err(err.into());
                        }
                    }
                };
                self.with_state(|st| {
                    st.received.push(Share {
                        group: gid.to_owned(),
                        file: fid.to_owned(),
                        path: path.clone(),
                        staged: false,
                    });
                    // Put on hold just too late.
                    if let Some(g) = st.groups.iter_mut().find(|g| g.id == gid) {
                        g.held_files.retain(|h| h != fid);
                    }
                });
                self.with_doc(gid, |d| d.mark_downloaded(fid, &me));
                self.end_transfer(gid, fid);
                self.emit(Event::Notice(format!("downloaded {}", offer.name)));
                self.doc_changed(gid, false);
                if self
                    .0
                    .store
                    .lock()
                    .unwrap()
                    .state
                    .groups
                    .iter()
                    .any(|g| g.id == gid && g.auto_rehost)
                {
                    // Fails when the sender stopped sharing in the meantime; nothing to host then.
                    let _ = self.host_file(gid, fid);
                }
                Ok(path)
            }
            Err(err) => {
                if offer.kind != APP_KIND {
                    self.emit(Event::Notice(format!(
                        "download of {} failed: {err:#}",
                        offer.name
                    )));
                }
                self.publish_group(gid, false);
                Err(err)
            }
        }
    }

    fn end_transfer(&self, gid: &str, fid: &str) {
        self.0
            .sharing
            .transfers
            .lock()
            .unwrap()
            .remove(&(gid.to_owned(), fid.to_owned()));
    }

    async fn fetch(
        &self,
        gid: &str,
        fid: &str,
        offer: &crate::groupdoc::FileOffer,
        target: Option<PathBuf>,
    ) -> Result<PathBuf> {
        let me = self.me();
        let conn = std::iter::once(&offer.from)
            .chain(
                offer
                    .hosts
                    .iter()
                    .filter(|h| **h != offer.from && **h != me),
            )
            .filter_map(|id| id.parse::<EndpointId>().ok())
            .find_map(|id| self.connection(id))
            .context("nobody who has it is online")?;
        let dir = match &target {
            Some(target) => target.parent().context("no folder")?.to_owned(),
            None => self.download_dir(),
        };
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("cannot create {}", dir.display()))?;

        let part = part_path(&dir, fid);
        // Older versions kept a folder's partial download as a directory; that cannot be resumed.
        if tokio::fs::metadata(&part).await.is_ok_and(|m| m.is_dir()) {
            let _ = tokio::fs::remove_dir_all(&part).await;
        }
        // What already arrived; a file of exactly the expected size is verified and renamed below.
        let existing = match tokio::fs::metadata(&part).await {
            Ok(m) if m.is_file() && m.len() > 0 => m.len(),
            _ => 0,
        };

        let (mut tx, mut rx) = conn.open_bi().await.map_err(|e| anyhow!("{e}"))?;
        let request = serde_json::to_vec(&DownloadRequest {
            group: gid.to_owned(),
            file: fid.to_owned(),
            from: existing,
            resume: true,
        })?;
        tx.write_all(&request).await.map_err(|e| anyhow!("{e}"))?;
        tx.finish().map_err(|e| anyhow!("{e}"))?;

        let mut header = [0u8; 9];
        rx.read_exact(&mut header)
            .await
            .map_err(|_| anyhow!("the sender refused or went away"))?;
        if header[0] == 1 {
            bail!("the sender no longer shares it");
        }
        let len = u64::from_be_bytes(header[1..].try_into().unwrap());
        // A resumed reply also says where it starts and how long the whole stream is. Without it
        // (an older sender), it starts at the beginning, and a folder's total is known only once
        // its listing has arrived.
        let (start, stream_len) = if header[0] == 2 {
            let mut more = [0u8; 16];
            rx.read_exact(&mut more)
                .await
                .map_err(|_| anyhow!("the sender went away"))?;
            (
                u64::from_be_bytes(more[..8].try_into().unwrap()),
                Some(u64::from_be_bytes(more[8..].try_into().unwrap())),
            )
        } else {
            (0, (offer.kind != FOLDER_KIND).then_some(len))
        };
        let folder = offer.kind == FOLDER_KIND;
        if len != offer.size {
            let _ = tokio::fs::remove_file(&part).await;
            bail!("the file changed on the sender's side");
        }
        if start > existing || stream_len.is_some_and(|total| total < start) {
            let _ = tokio::fs::remove_file(&part).await;
            bail!("the sender started at an impossible place");
        }
        if start != existing {
            // The sender ignored our offset (an older version) or could not honor it: keep only
            // what it is about to resend.
            match tokio::fs::OpenOptions::new().write(true).open(&part).await {
                Ok(file) => file.set_len(start).await?,
                Err(_) if start == 0 => {}
                Err(err) => return Err(err.into()),
            }
        }

        let started = Instant::now();
        let mut body = ZstdDecoder::new(BufReader::with_capacity(CHUNK, &mut rx));
        // A folder's stream starts with its listing, which is not part of the progress.
        let prefix = if folder {
            listing_len(&part, start).await
        } else {
            0
        };
        let hash = self
            .receive_flat(gid, fid, &mut body, &part, start, stream_len, prefix)
            .await?;
        if hash != offer.hash {
            let _ = tokio::fs::remove_file(&part).await;
            bail!("the file arrived corrupted");
        }
        if folder {
            if let Some(total) = stream_len
                && tokio::fs::metadata(&part).await?.len() != total
            {
                let _ = tokio::fs::remove_file(&part).await;
                bail!("the folder arrived incomplete");
            }
            let target = target.unwrap_or_else(|| unique_path(&dir, &offer.name));
            if let Err(err) = Self::unpack_folder(&part, &target).await {
                let _ = tokio::fs::remove_dir_all(&target).await;
                return Err(err);
            }
            let _ = tokio::fs::remove_file(&part).await;
            tracing::info!(
                "received {} ({len} bytes in {:?}), route {}",
                offer.name,
                started.elapsed(),
                super::route(&conn)
            );
            return Ok(target);
        }
        if tokio::fs::metadata(&part).await?.len() != offer.size {
            bail!("the file arrived incomplete");
        }
        tracing::info!(
            "received {} ({len} bytes in {:?}), route {}",
            offer.name,
            started.elapsed(),
            super::route(&conn)
        );
        let target = target.unwrap_or_else(|| unique_path(&dir, &offer.name));
        tokio::fs::rename(&part, &target).await?;
        Ok(target)
    }

    /// Writes the (decompressed) stream to `part`, starting at `start`, and returns the hash of the
    /// whole thing (the bytes already there included). `total` is the full stream's length when the
    /// sender told us. `prefix` is a folder's listing, which is not part of the progress.
    async fn receive_flat(
        &self,
        gid: &str,
        fid: &str,
        body: &mut (impl AsyncRead + Unpin),
        part: &Path,
        start: u64,
        total: Option<u64>,
        prefix: u64,
    ) -> Result<String> {
        let lost = |e: std::io::Error| anyhow!("connection lost: {e}");
        let mut hasher = blake3::Hasher::new();
        let mut buf = vec![0u8; CHUNK];
        // Hash what a previous attempt left us, so the final check covers the whole file.
        if start > 0 {
            let mut old = tokio::fs::File::open(part).await?;
            old.set_max_buf_size(CHUNK);
            let mut left = start;
            while left > 0 {
                let want = left.min(CHUNK as u64) as usize;
                let n = old.read(&mut buf[..want]).await.map_err(lost)?;
                if n == 0 {
                    bail!("the partial file is shorter than expected");
                }
                hasher.update(&buf[..n]);
                left -= n as u64;
            }
        }
        let mut out = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .open(part)
            .await?;
        out.set_max_buf_size(CHUNK);
        out.set_len(start).await?;
        out.seek(SeekFrom::Start(start)).await?;
        let (mut done, mut shown) = (start, Instant::now());
        loop {
            let n = body.read(&mut buf).await.map_err(lost)?;
            if n == 0 {
                break;
            }
            if let Some(total) = total
                && done + n as u64 > total
            {
                bail!("the sender sent more than the file holds");
            }
            hasher.update(&buf[..n]);
            out.write_all(&buf[..n]).await?;
            done += n as u64;
            self.progressed(gid, fid, done.saturating_sub(prefix), &mut shown);
        }
        out.flush().await?;
        if let Some(total) = total
            && done != total
        {
            bail!("connection lost after {done} of {total} bytes");
        }
        Ok(hasher.finalize().to_hex().to_string())
    }

    /// Unpacks a whole folder (its stream, as written by [`receive_flat`]) into `target`.
    async fn unpack_folder(part: &Path, target: &Path) -> Result<()> {
        let lost = |e: std::io::Error| anyhow!("the download is incomplete: {e}");
        let mut src = tokio::fs::File::open(part).await?;
        let mut head = [0u8; 4];
        src.read_exact(&mut head).await.map_err(lost)?;
        let n = u32::from_be_bytes(head) as usize;
        if n > MAX_LISTING_BYTES {
            bail!("the folder's listing is too big");
        }
        let mut raw = vec![0u8; n];
        src.read_exact(&mut raw).await.map_err(lost)?;
        let listing: Listing = serde_json::from_slice(&raw).context("bad folder listing")?;

        tokio::fs::create_dir_all(target).await?;
        for dir in &listing.dirs {
            tokio::fs::create_dir_all(target.join(safe_relative(dir)?)).await?;
        }
        let mut buf = vec![0u8; CHUNK];
        for (rel, size) in &listing.files {
            let path = target.join(safe_relative(rel)?);
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            // `create_new`: a name listed twice must not write over (or through) the first one.
            let mut out = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .await
                .with_context(|| format!("cannot create {rel}"))?;
            out.set_max_buf_size(CHUNK);
            let mut left = *size;
            while left > 0 {
                let want = left.min(CHUNK as u64) as usize;
                let n = src.read(&mut buf[..want]).await.map_err(lost)?;
                if n == 0 {
                    bail!("the folder arrived incomplete");
                }
                out.write_all(&buf[..n]).await?;
                left -= n as u64;
            }
            out.flush().await?;
        }
        if src.read(&mut buf).await.map_err(lost)? != 0 {
            bail!("the sender sent more than the folder holds");
        }
        Ok(())
    }

    /// A download got to `done` bytes: show it (at most every 200 ms).
    fn progressed(&self, gid: &str, fid: &str, done: u64, shown: &mut Instant) {
        if let Some(t) = self
            .0
            .sharing
            .transfers
            .lock()
            .unwrap()
            .get_mut(&(gid.to_owned(), fid.to_owned()))
        {
            t.done = done;
        }
        if shown.elapsed() > Duration::from_millis(200) {
            *shown = Instant::now();
            self.publish_group(gid, false);
        }
    }

    /// The other side of [`Core::fetch`].
    pub(super) async fn serve_download(
        &self,
        from: EndpointId,
        mut tx: SendStream,
        mut rx: RecvStream,
    ) {
        // Kept until the other side has it all, so the sender's bar stays until it really is done.
        let _sending = match self.serve(from, &mut tx, &mut rx).await {
            Ok(sending) => sending,
            Err(err) => {
                tracing::debug!("refused download to {}: {err:#}", from.fmt_short());
                let _ = tx.write_all(&[1u8; 9]).await;
                None
            }
        };
        let _ = tx.finish();
        let _ = tx.stopped().await;
    }

    async fn serve(
        &self,
        from: EndpointId,
        tx: &mut SendStream,
        rx: &mut RecvStream,
    ) -> Result<Option<SendMeter>> {
        let raw = rx.read_to_end(4096).await?;
        if raw == b"ping" {
            // Liveness probe (see `Core::drop_stale`).
            tx.write_all(&[0xFF]).await?;
            return Ok(None);
        }
        let request: DownloadRequest = serde_json::from_slice(&raw)?;
        if !self.has_group(&request.group) || !self.group_peers(&request.group).contains(&from) {
            bail!("not a member");
        }
        let me = self.me();
        let (offer, members) =
            self.with_doc(&request.group, |d| (d.file(&request.file), d.members()));
        let offer = offer
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
        let to_name = members
            .into_iter()
            .find(|(id, _)| *id == from.to_string())
            .map(|(_, name)| name)
            .unwrap_or_else(|| from.fmt_short().to_string());
        let sending = self.track_send(
            &request.group,
            &request.file,
            &offer.name,
            &from.to_string(),
            &to_name,
            offer.size,
        );
        if let Some(conn) = self.connection(from) {
            tracing::info!(
                "sending {} to {}, route {}",
                offer.name,
                from.fmt_short(),
                super::route(&conn)
            );
        }
        let folder = offer.kind == FOLDER_KIND;
        // What travels, in order: a folder's listing, then its files' bytes.
        let (listing, listing_len, stream_len) = if folder {
            let walk = {
                let path = path.clone();
                tokio::task::spawn_blocking(move || walk_folder(&path))
            }
            .await??;
            let bytes: u64 = walk.0.files.iter().map(|f| f.1).sum();
            if bytes != offer.size {
                bail!("folder changed since it was offered");
            }
            let listing_len = walk.0.encode().len() as u64;
            (Some(walk), listing_len, listing_len + bytes)
        } else {
            (None, 0, offer.size)
        };
        let start = if request.resume {
            request.from.min(stream_len)
        } else {
            0
        };
        // A resumed send starts part-way, so the sender's bar does too.
        sending.set_done(start.saturating_sub(listing_len));
        let mut header = vec![if request.resume { 2u8 } else { 0u8 }];
        header.extend_from_slice(&offer.size.to_be_bytes());
        if request.resume {
            header.extend_from_slice(&start.to_be_bytes());
            header.extend_from_slice(&stream_len.to_be_bytes());
        }
        tx.write_all(&header).await?;
        if start >= stream_len {
            return Ok(Some(sending));
        }
        if let Some((listing, paths)) = listing {
            serve_folder(listing, paths, listing_len, start, tx, &sending).await?;
            return Ok(Some(sending));
        }
        let mut file = tokio::fs::File::from_std(
            tokio::task::spawn_blocking(move || open_source(&path)).await??,
        );
        file.set_max_buf_size(CHUNK);
        let len = file.metadata().await?.len();
        if len != offer.size {
            bail!("file changed since it was offered");
        }
        file.seek(SeekFrom::Start(start)).await?;
        let body = ZstdEncoder::with_quality(
            BufReader::with_capacity(CHUNK, sending.counted(file)),
            Level::Precise(1),
        );
        tokio::io::copy_buf(&mut BufReader::with_capacity(CHUNK, body), tx).await?;
        Ok(Some(sending))
    }

    // ---- automatic behaviour ----------------------------------------------------------

    pub(super) fn auto_jobs_all(&self) {
        let groups: Vec<String> = self
            .0
            .store
            .lock()
            .unwrap()
            .state
            .groups
            .iter()
            .map(|g| g.id.clone())
            .collect();
        for gid in groups {
            self.auto_jobs(&gid);
        }
    }

    /// Fetch an offer automatically. They go one at a time, in the order they were queued, so a
    /// group offering many files does not fetch them all at once.
    fn auto_download(&self, gid: &str, fid: &str, name: String, size: u64, from: String) {
        let key = (gid.to_owned(), fid.to_owned());
        let started = Arc::new(AtomicBool::new(false));
        let queued = Queued {
            name,
            size,
            from,
            started: started.clone(),
        };
        if self
            .0
            .sharing
            .auto_queued
            .lock()
            .unwrap()
            .insert(key.clone(), queued)
            .is_some()
        {
            // A job ran again while this one still waits (or downloads).
            return;
        }
        self.publish_transfers();
        let this = self.clone();
        tokio::spawn(async move {
            let Ok(_turn) = this.0.sharing.auto_turn.acquire().await else {
                this.0.sharing.auto_queued.lock().unwrap().remove(&key);
                this.publish_transfers();
                return;
            };
            started.store(true, Ordering::Relaxed);
            // The group's settings may have changed while it waited, and it may have been put on
            // hold or downloaded by hand.
            let wanted = this
                .0
                .store
                .lock()
                .unwrap()
                .state
                .groups
                .iter()
                .find(|g| g.id == key.0)
                .is_some_and(|g| {
                    g.auto_download
                        && !g.held_files.contains(&key.1)
                        && !g.hidden_files.contains(&key.1)
                });
            if wanted {
                // Errors are shown on the file and retried later.
                let _ = this.download(&key.0, &key.1, false).await;
            }
            this.0.sharing.auto_queued.lock().unwrap().remove(&key);
            this.publish_transfers();
        });
    }

    /// Per-group local settings at work: download new offers, stop sharing what everybody has. And
    /// apps, which every member fetches and serves whatever their settings.
    pub(super) fn auto_jobs(&self, gid: &str) {
        self.app_jobs(gid);
        let Some((auto_download, auto_stop, hidden, held)) = self
            .0
            .store
            .lock()
            .unwrap()
            .state
            .groups
            .iter()
            .find(|g| g.id == gid)
            .map(|g| {
                (
                    g.auto_download,
                    g.auto_stop,
                    g.hidden_files.clone(),
                    g.held_files.clone(),
                )
            })
        else {
            return;
        };
        if !auto_download && !auto_stop {
            return;
        }
        let me = self.me();
        let (members, files) = self.with_doc(gid, |d| (d.members(), d.files()));
        // Apps' files come and go with their tabs (see `app_jobs`).
        for f in files.iter().filter(|f| !f.stopped && f.kind != APP_KIND) {
            if f.from == me {
                let audience: Vec<&String> = members
                    .iter()
                    .map(|(id, _)| id)
                    .filter(|id| **id != me)
                    .collect();
                if auto_stop
                    && !audience.is_empty()
                    && audience.iter().all(|id| f.downloaded.contains_key(*id))
                {
                    let _ = self.stop_file(gid, &f.id);
                }
            } else if auto_download
                && !f.downloaded.contains_key(&me)
                && !hidden.contains(&f.id)
                && !held.contains(&f.id)
            {
                let online = std::iter::once(&f.from)
                    .chain(&f.hosts)
                    .filter(|id| **id != me)
                    .any(|id| id.parse().ok().and_then(|id| self.connection(id)).is_some());
                if online {
                    let from = if f.from_name.is_empty() {
                        members
                            .iter()
                            .find(|(id, _)| *id == f.from)
                            .map(|(_, name)| name.clone())
                            .unwrap_or_else(|| f.from.clone())
                    } else {
                        f.from_name.clone()
                    };
                    self.auto_download(gid, &f.id, f.name.clone(), f.size, from);
                }
            }
        }
    }
}

/// Does the peer on this connection answer? A connection to a peer that restarted looks alive
/// until QUIC's idle timeout, so ask.
/// Open what a share points at: a file path, or on Android a `content://` URI the user picked.
/// blake3 (hex) and size of the file at `path`; for a folder, of the bytes it travels as.
async fn hash_source(path: PathBuf, meter: Arc<Meter>) -> Result<(String, u64)> {
    tokio::task::spawn_blocking(move || -> Result<(String, u64)> {
        if path.is_dir() {
            return hash_folder(&path, &meter);
        }
        let mut file =
            open_source(&path).with_context(|| format!("cannot read {}", path.display()))?;
        if !file.metadata()?.is_file() {
            bail!("{} is not a file", path.display());
        }
        meter.total.store(file.metadata()?.len(), Ordering::Relaxed);
        let mut hashing = Hashing::new(&meter);
        let size = std::io::copy(&mut file, &mut hashing)?;
        Ok((hashing.hex(), size))
    })
    .await?
}

/// Hashes what is written to it, keeping count on a [`Meter`].
struct Hashing<'a> {
    hasher: blake3::Hasher,
    meter: &'a Meter,
}

impl<'a> Hashing<'a> {
    fn new(meter: &'a Meter) -> Self {
        Hashing {
            hasher: blake3::Hasher::new(),
            meter,
        }
    }

    fn hex(&self) -> String {
        self.hasher.finalize().to_hex().to_string()
    }
}

impl Write for Hashing<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.hasher.update(buf);
        self.meter
            .done
            .fetch_add(buf.len() as u64, Ordering::Relaxed);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Publishes a group's view every so often, until dropped.
struct Ticker(tokio::task::AbortHandle);

impl Drop for Ticker {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// While alive, an offer being hashed shows in its group.
struct PreparingGuard {
    core: Core,
    group: String,
    id: String,
    _ticker: Ticker,
}

impl Drop for PreparingGuard {
    fn drop(&mut self) {
        let sharing = &self.core.0.sharing;
        sharing
            .preparing
            .lock()
            .unwrap()
            .retain(|p| p.id != self.id);
        self.core.publish_group(&self.group, false);
    }
}

/// While alive, a file going to someone shows in its group.
pub(super) struct SendMeter {
    core: Core,
    group: String,
    done: Arc<AtomicU64>,
    _ticker: Ticker,
}

impl SendMeter {
    /// `reader`, with what is read from it counted as sent.
    pub(super) fn counted<R>(&self, reader: R) -> Counted<R> {
        Counted {
            reader,
            done: self.done.clone(),
        }
    }

    /// Count a prefix that is not read now (a resumed send).
    fn set_done(&self, done: u64) {
        self.done.store(done, Ordering::Relaxed);
    }
}

impl Drop for SendMeter {
    fn drop(&mut self) {
        let sharing = &self.core.0.sharing;
        sharing
            .sending
            .lock()
            .unwrap()
            .retain(|s| !Arc::ptr_eq(&s.done, &self.done));
        self.core.publish_group(&self.group, false);
    }
}

/// While alive, a guest's upload counts as moving.
pub(super) struct IncomingMeter {
    core: Core,
    group: String,
    done: Arc<AtomicU64>,
    _ticker: Ticker,
}

impl IncomingMeter {
    pub(super) fn add(&self, bytes: usize) {
        self.done.fetch_add(bytes as u64, Ordering::Relaxed);
    }
}

impl Drop for IncomingMeter {
    fn drop(&mut self) {
        let sharing = &self.core.0.sharing;
        sharing
            .incoming
            .lock()
            .unwrap()
            .retain(|up| !Arc::ptr_eq(&up.done, &self.done));
        self.core.publish_group(&self.group, false);
    }
}

/// See [`SendMeter::counted`].
pub(super) struct Counted<R> {
    reader: R,
    done: Arc<AtomicU64>,
}

impl<R: AsyncRead + Unpin> AsyncRead for Counted<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.reader).poll_read(cx, buf);
        self.done
            .fetch_add((buf.filled().len() - before) as u64, Ordering::Relaxed);
        polled
    }
}

/// What a shared folder holds, in the order its files travel: its directories (so empty ones come
/// along too) and its files, as `/`-separated paths relative to the folder. Symlinks and special
/// files are left out.
#[derive(Serialize, Deserialize, Default)]
struct Listing {
    dirs: Vec<String>,
    files: Vec<(String, u64)>,
}

impl Listing {
    /// The `[length u32 BE][JSON]` a folder's bytes start with.
    fn encode(&self) -> Vec<u8> {
        let json = serde_json::to_vec(self).expect("listing serializes");
        let mut out = (json.len() as u32).to_be_bytes().to_vec();
        out.extend(json);
        out
    }
}

/// Lists the folder at `root`, sorted; also returns where each listed file is.
fn walk_folder(root: &Path) -> Result<(Listing, Vec<PathBuf>)> {
    let (mut dirs, mut files) = (Vec::new(), Vec::new());
    let mut todo = vec![(root.to_owned(), String::new())];
    while let Some((dir, rel)) = todo.pop() {
        for entry in
            std::fs::read_dir(&dir).with_context(|| format!("cannot read {}", dir.display()))?
        {
            let entry = entry?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .with_context(|| format!("{} has an unreadable name", entry.path().display()))?;
            let rel = if rel.is_empty() {
                name.to_owned()
            } else {
                format!("{rel}/{name}")
            };
            let meta = entry.metadata()?; // does not follow symlinks
            if meta.is_dir() {
                dirs.push(rel.clone());
                todo.push((entry.path(), rel));
            } else if meta.is_file() {
                files.push((rel, meta.len(), entry.path()));
            }
        }
    }
    dirs.sort();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let paths = files.iter().map(|f| f.2.clone()).collect();
    let files = files
        .into_iter()
        .map(|(rel, size, _)| (rel, size))
        .collect();
    Ok((Listing { dirs, files }, paths))
}

fn hash_folder(root: &Path, meter: &Meter) -> Result<(String, u64)> {
    let (listing, paths) = walk_folder(root)?;
    let expected = listing.files.iter().map(|f| f.1).sum();
    meter.total.store(expected, Ordering::Relaxed);
    let mut hashing = Hashing::new(meter);
    // The listing is hashed too, but it is not what the sizes count.
    hashing.hasher.update(&listing.encode());
    let mut total = 0;
    for (path, (_, size)) in paths.iter().zip(&listing.files) {
        let file =
            std::fs::File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
        if std::io::copy(&mut std::io::Read::take(file, *size), &mut hashing)? != *size {
            bail!("{} changed while reading it", path.display());
        }
        total += size;
    }
    Ok((hashing.hex(), total))
}

/// Sends the folder (see [`Listing`]) from `start`, compressed.
async fn serve_folder(
    listing: Listing,
    paths: Vec<PathBuf>,
    listing_len: u64,
    start: u64,
    tx: &mut SendStream,
    sending: &SendMeter,
) -> Result<()> {
    let mut body = async_compression::tokio::write::ZstdEncoder::with_quality(
        BufWriter::with_capacity(CHUNK, &mut *tx),
        Level::Precise(1),
    );
    if start < listing_len {
        let bytes = listing.encode();
        let from = start.min(bytes.len() as u64) as usize;
        body.write_all(&bytes[from..]).await?;
    }
    let mut pos = listing_len;
    for (path, (_, size)) in paths.iter().zip(&listing.files) {
        if pos + size <= start {
            pos += size;
            continue;
        }
        let skip = start.saturating_sub(pos).min(*size);
        let mut file = tokio::fs::File::open(path).await?;
        file.set_max_buf_size(CHUNK);
        if skip > 0 {
            file.seek(SeekFrom::Start(skip)).await?;
        }
        let left = *size - skip;
        let mut reader = BufReader::with_capacity(CHUNK, sending.counted(file.take(left)));
        if tokio::io::copy_buf(&mut reader, &mut body).await? != left {
            bail!("folder changed since it was offered");
        }
        pos += *size;
    }
    body.shutdown().await?;
    Ok(())
}

/// A `/`-separated path from a folder listing, as a relative path that stays inside the folder.
fn safe_relative(rel: &str) -> Result<PathBuf> {
    let mut out = PathBuf::new();
    for part in rel.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.contains(['\\', ':', '\0']) {
            bail!("unsafe path in the folder: {rel:?}");
        }
        out.push(part);
    }
    Ok(out)
}

/// Where a download is put together until it is complete.
fn part_path(dir: &Path, fid: &str) -> PathBuf {
    dir.join(format!(".{fid}.part"))
}

/// How many bytes the folder listing at the start of `part` takes, if it has already arrived.
async fn listing_len(part: &Path, have: u64) -> u64 {
    if have < 4 {
        return 0;
    }
    let Ok(mut file) = tokio::fs::File::open(part).await else {
        return 0;
    };
    let mut head = [0u8; 4];
    if file.read_exact(&mut head).await.is_err() {
        return 0;
    }
    let n = u32::from_be_bytes(head) as u64;
    if 4 + n <= have { 4 + n } else { 0 }
}

pub(super) fn open_source(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(target_os = "android")]
    if path.to_str().is_some_and(|p| p.starts_with("content://")) {
        let fd = crate::platform::open_content_fd(&path.to_string_lossy())
            .ok_or_else(|| std::io::Error::other("the picked file is no longer readable"))?;
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
                let still_used = self
                    .0
                    .store
                    .lock()
                    .unwrap()
                    .state
                    .shares
                    .iter()
                    .any(|o| o.path == s.path)
                    || self
                        .0
                        .sharing
                        .preparing
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|p| p.path == s.path);
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
    tokio::time::timeout(Duration::from_secs(2), ask)
        .await
        .is_ok_and(|r| r.is_some())
}

impl Core {
    /// An incoming connection is about to lose the duplicate tie-break against a connection we
    /// already hold. If that one is dead (the peer restarted), close it so the new one is kept.
    pub(super) async fn drop_stale(&self, incoming: &Connection) {
        let id = incoming.remote_id();
        if id < self.id() {
            return; // the incoming connection wins the tie-break anyway
        }
        let Some(existing) = self.connection(id) else {
            return;
        };
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
    let name = Path::new(name)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name: String = name
        .chars()
        .filter(|c| !c.is_control() && *c != '\\')
        .collect();
    let name = name.trim();
    if name.is_empty() {
        "file".into()
    } else {
        name.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfers_are_described() {
        let one = TransfersNow {
            receiving: 1,
            name: Some("a.zip".into()),
            done: 512,
            total: Some(1024),
            ..TransfersNow::default()
        };
        let (title, detail, percent) = one.describe();
        assert_eq!(title, "Downloading a.zip");
        assert!(detail.contains(" of "), "{detail}");
        assert_eq!(percent, 50);

        let many = TransfersNow {
            receiving: 2,
            sending: 1,
            done: 0,
            total: None,
            ..TransfersNow::default()
        };
        assert_eq!(
            many.describe(),
            (
                "Downloading 2 files, sending 1 file".to_owned(),
                String::new(),
                -1
            )
        );
        assert!(many.active());
        assert!(!TransfersNow::default().active());
    }

    #[test]
    fn folder_paths_stay_inside() {
        assert_eq!(
            safe_relative("a/b c/d.txt").unwrap(),
            Path::new("a").join("b c").join("d.txt")
        );
        for bad in [
            "",
            "/etc/passwd",
            "a//b",
            "../x",
            "a/../../x",
            "./a",
            "a/",
            "C:/x",
            "a\\..\\x",
        ] {
            assert!(safe_relative(bad).is_err(), "{bad:?}");
        }
    }
}
