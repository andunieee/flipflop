//! Guest links: a group that anyone holding a link joins from a browser, no app needed.
//!
//! A guest group is an ordinary group (its own Automerge document, listed with the others) that also has a
//! [`GuestLink`] in the state: a secret token and whether guests may upload. Guests are not iroh peers but
//! sessions: the page keeps its own copy of the document and syncs it with ours, over one of two transports
//! that carry the same messages:
//! - on the LAN, a WebSocket to the small HTTP server that also serves the page ([`http`]);
//! - from anywhere, an iroh connection from the page on GitHub Pages (ALPN [`GUEST_ALPN`]); its first
//!   bidirectional stream carries the messages, every later one an HTTP request (downloads, uploads).
//!
//! A message is a tag byte and a payload: [`TAG_CONTROL`] + JSON, or [`TAG_SYNC`] + an Automerge sync message.
//! Over iroh each message is prefixed with its length (u32, big endian); a WebSocket frames them itself.
//!
//! A guest is a member of the document under an id made from where it came from (its iroh key, or its LAN
//! address) and the name derived from that ("beautiful-iguana"). We write that member entry ourselves, since
//! guests do not choose who they are. Whoever holds the link can read and change the whole document of the
//! group, so a guest group is a place for guests only: never invite a contact into it you would not hand the
//! link to.

mod http;

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use anyhow::{Context, Result, bail};
use iroh::EndpointId;
use serde_json::json;
use tokio::{io::AsyncWriteExt, sync::mpsc, task::JoinHandle};

use super::{Core, new_group_id, sharing};
use crate::{
    groupdoc::random_hex,
    haiku,
    state::{Group, GuestLink},
};

pub(super) use http::serve_tunnel;


pub const GUEST_ALPN: &[u8] = b"flipflop/guest/1";
/// The port of the LAN server when it is free (any free port otherwise).
const LAN_PORT: u16 = 53530;
/// Where the page for links from anywhere is published (GitHub Pages).
const WEB_PAGE: &str = "https://andunieee.github.io/flipflop/";
const TAG_CONTROL: u8 = 0;
const TAG_SYNC: u8 = 1;
/// Biggest message accepted from a guest.
const MAX_FRAME: usize = 32 * 1024 * 1024;

/// What a guest session is told to do by the rest of the node.
pub(super) enum Out {
    Frame(Vec<u8>),
    Close,
}

/// Where a guest came from: it decides who they are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Origin {
    Lan(IpAddr),
    Iroh(EndpointId),
}

impl Origin {
    /// Their member id in the document.
    pub(super) fn id(&self) -> String {
        match self {
            Origin::Lan(ip) => format!("guest:lan:{ip}"),
            Origin::Iroh(id) => format!("guest:{id}"),
        }
    }

    pub(super) fn name(&self) -> String {
        haiku::guest_name(&self.id())
    }
}

/// Is this member id a guest's?
pub fn is_guest(member: &str) -> bool {
    member.starts_with("guest:")
}

struct Session {
    guest: String,
    tx: mpsc::UnboundedSender<Out>,
}

struct Abort(JoinHandle<()>);

impl Drop for Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct LanServer {
    port: u16,
    ip: Option<Ipv4Addr>,
    _task: Abort,
}

#[derive(Default)]
pub(super) struct Guests {
    /// Per group: session key (`<guest id>#<serial>`, also the key of its sync state) -> session.
    sessions: Mutex<HashMap<String, HashMap<String, Session>>>,
    serial: AtomicU64,
    server: Mutex<Option<LanServer>>,
    starting: AtomicBool,
}

/// What the UI shows of a guest group.
#[derive(Debug, Clone, PartialEq)]
pub struct GuestInfo {
    pub token: String,
    pub uploads: bool,
    /// The link for browsers on the same network (while the server is up).
    pub lan_url: Option<String>,
    /// The link for browsers anywhere (once we have a home relay).
    pub web_url: Option<String>,
    pub guests: Vec<GuestPerson>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GuestPerson {
    pub name: String,
    pub online: bool,
}

/// A live guest session; dropping it ends it.
pub(super) struct GuestSession {
    core: Core,
    gid: String,
    key: String,
}

impl Drop for GuestSession {
    fn drop(&mut self) {
        if let Some(group) = self.core.0.guests.sessions.lock().unwrap().get_mut(&self.gid) {
            group.remove(&self.key);
        }
        self.core.publish_group(&self.gid, false);
    }
}

fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// `%XX`-encode everything but unreserved characters.
fn encode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

impl Core {
    // ---- links ------------------------------------------------------------------------

    /// Make a guest group with a fresh link. Returns the group's id.
    pub fn create_guest_link(&self, uploads: bool) -> String {
        let id = new_group_id();
        let token = random_hex(16);
        self.with_state(|st| {
            let mut g = Group::new(id.clone(), Vec::new());
            g.guest = Some(GuestLink { token, uploads });
            st.groups.push(g);
        });
        self.join_doc(&id);
        self.ensure_lan_server();
        id
    }

    /// Turn an ordinary group into a guest group with a fresh link. Its members stay. Does nothing
    /// (and returns false) for an implied group or a group that already has a link.
    pub fn enable_guest_link(&self, gid: &str, uploads: bool) -> bool {
        let token = random_hex(16);
        let enabled = self.with_state(|st| match st.groups.iter_mut().find(|g| g.id == gid && g.with.is_none() && g.guest.is_none()) {
            Some(g) => {
                g.guest = Some(GuestLink { token, uploads });
                true
            }
            None => false,
        });
        if enabled {
            self.ensure_lan_server();
        }
        enabled
    }

    /// Allow or forbid uploads on a guest link; the pages are told right away.
    pub fn set_guest_uploads(&self, gid: &str, on: bool) {
        let changed = self.with_state(|st| match st.groups.iter_mut().find(|g| g.id == gid).and_then(|g| g.guest.as_mut()) {
            Some(link) => std::mem::replace(&mut link.uploads, on) != on,
            None => false,
        });
        if changed {
            self.guest_broadcast(gid, TAG_CONTROL, json!({"t": "uploads", "on": on}).to_string().as_bytes());
        }
    }

    /// Close a guest link: its guests are disconnected and the group is gone.
    pub fn close_guest_link(&self, gid: &str) {
        if !self.0.store.lock().unwrap().state.groups.iter().any(|g| g.id == gid && g.guest.is_some()) {
            return;
        }
        self.guest_broadcast(gid, TAG_CONTROL, json!({"t": "closed"}).to_string().as_bytes());
        if let Some(sessions) = self.0.guests.sessions.lock().unwrap().remove(gid) {
            for s in sessions.values() {
                let _ = s.tx.send(Out::Close);
            }
        }
        self.drop_doc(gid);
        self.with_state(|st| st.groups.retain(|g| g.id != gid));
        if !self.0.store.lock().unwrap().state.groups.iter().any(|g| g.guest.is_some()) {
            *self.0.guests.server.lock().unwrap() = None;
            self.publish();
        }
    }

    /// The group a token opens, and whether it lets guests upload.
    pub(super) fn guest_group(&self, token: &str) -> Option<(String, bool)> {
        let store = self.0.store.lock().unwrap();
        store.state.groups.iter().find_map(|g| {
            let link = g.guest.as_ref()?;
            same(&link.token, token).then(|| (g.id.clone(), link.uploads))
        })
    }

    fn guest_uploads(&self, gid: &str) -> bool {
        self.0.store.lock().unwrap().state.groups.iter().find(|g| g.id == gid).and_then(|g| g.guest.as_ref()).is_some_and(|l| l.uploads)
    }

    // ---- the LAN server ---------------------------------------------------------------

    /// Start the LAN server if it is not running (it only runs while a guest link exists).
    pub(super) fn ensure_lan_server(&self) {
        let guests = &self.0.guests;
        if guests.server.lock().unwrap().is_some() || guests.starting.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            let bound = match tokio::net::TcpListener::bind(("0.0.0.0", LAN_PORT)).await {
                Ok(l) => Ok(l),
                Err(_) => tokio::net::TcpListener::bind(("0.0.0.0", 0)).await,
            };
            match bound {
                Ok(listener) => {
                    let port = listener.local_addr().map_or(LAN_PORT, |a| a.port());
                    let task = Abort(tokio::spawn(http::serve(listener, this.clone())));
                    *this.0.guests.server.lock().unwrap() = Some(LanServer { port, ip: lan_ip(), _task: task });
                }
                Err(err) => tracing::warn!("guest server could not start: {err}"),
            }
            this.0.guests.starting.store(false, Ordering::SeqCst);
            this.publish();
        });
    }

    pub(super) fn stop_lan_server(&self) {
        *self.0.guests.server.lock().unwrap() = None;
    }

    /// What the UI needs of a guest group.
    pub(super) fn guest_info(&self, gid: &str, link: &GuestLink) -> GuestInfo {
        let lan_url = self.0.guests.server.lock().unwrap().as_ref().and_then(|s| Some(format!("http://{}:{}/{}/", s.ip?, s.port, link.token)));
        let web_url = self.0.endpoint.addr().relay_urls().next().map(|relay| {
            format!("{WEB_PAGE}#t={}&e={}&r={}", link.token, self.id(), encode(&relay.to_string()))
        });
        let online = self.guests_online(gid);
        let guests = self
            .with_doc(gid, |d| d.members())
            .into_iter()
            .filter(|(id, _)| is_guest(id))
            .map(|(id, name)| GuestPerson { online: online.contains(&id), name })
            .collect();
        GuestInfo { token: link.token.clone(), uploads: link.uploads, lan_url, web_url, guests }
    }

    /// Member ids of the guests connected to a group right now.
    pub(super) fn guests_online(&self, gid: &str) -> std::collections::HashSet<String> {
        self.0.guests.sessions.lock().unwrap().get(gid).map(|g| g.values().map(|s| s.guest.clone()).collect()).unwrap_or_default()
    }

    // ---- sessions ---------------------------------------------------------------------

    /// A guest connected: record them in the document, say hello and start syncing. Frames for them go
    /// through `tx`.
    pub(super) fn guest_attach(&self, gid: &str, origin: &Origin, tx: mpsc::UnboundedSender<Out>) -> Option<GuestSession> {
        let (title, uploads) = {
            let store = self.0.store.lock().unwrap();
            let g = store.state.groups.iter().find(|g| g.id == gid)?;
            (g.alias.clone(), g.guest.as_ref()?.uploads)
        };
        let (guest, name) = (origin.id(), origin.name());
        let key = format!("{guest}#{}", self.0.guests.serial.fetch_add(1, Ordering::SeqCst));
        self.with_doc(gid, |d| {
            d.join(&guest, &name);
            d.reset_sync(&key);
        });
        let hello = json!({
            "t": "hello", "id": guest, "name": name, "uploads": uploads,
            "group": title.unwrap_or_else(|| super::parse_gid(gid).map(|g| haiku::name(&g)).unwrap_or_default()),
            "host": self.name(),
        });
        let mut frame = vec![TAG_CONTROL];
        frame.extend_from_slice(hello.to_string().as_bytes());
        let _ = tx.send(Out::Frame(frame));
        self.0.guests.sessions.lock().unwrap().entry(gid.to_owned()).or_default().insert(key.clone(), Session { guest: guest.clone(), tx });
        self.doc_changed(gid, false);
        Some(GuestSession { core: self.clone(), gid: gid.to_owned(), key })
    }

    pub(super) fn guest_broadcast(&self, gid: &str, tag: u8, payload: &[u8]) {
        let mut frame = vec![tag];
        frame.extend_from_slice(payload);
        if let Some(group) = self.0.guests.sessions.lock().unwrap().get(gid) {
            for s in group.values() {
                let _ = s.tx.send(Out::Frame(frame.clone()));
            }
        }
    }

    /// A message from a guest.
    pub(super) fn guest_frame(&self, session: &GuestSession, frame: &[u8]) {
        let Some((&tag, payload)) = frame.split_first() else { return };
        if tag != TAG_SYNC || frame.len() > MAX_FRAME {
            return;
        }
        match self.with_doc(&session.gid, |d| d.receive_sync(&session.key, payload)) {
            Ok(true) => self.doc_changed(&session.gid, false),
            Ok(false) => self.sync_guest(&session.gid, &session.key),
            Err(err) => tracing::debug!("sync from a guest failed: {err}"),
        }
    }

    /// Tell every guest session of a group what it is missing.
    pub(super) fn sync_guests(&self, gid: &str) {
        let keys: Vec<String> = self.0.guests.sessions.lock().unwrap().get(gid).map(|g| g.keys().cloned().collect()).unwrap_or_default();
        for key in keys {
            self.sync_guest(gid, &key);
        }
    }

    fn sync_guest(&self, gid: &str, key: &str) {
        for _ in 0..16 {
            let Some(message) = self.with_doc(gid, |d| d.sync_message(key)) else { break };
            let mut frame = vec![TAG_SYNC];
            frame.extend_from_slice(&message);
            let sent = self.0.guests.sessions.lock().unwrap().get(gid).and_then(|g| g.get(key)).map(|s| s.tx.send(Out::Frame(frame)).is_ok());
            if sent != Some(true) {
                break;
            }
        }
    }

    // ---- files ------------------------------------------------------------------------

    /// Where the bytes of an offer are on this device, for a guest to download.
    pub(super) fn guest_file(&self, gid: &str, fid: &str) -> Result<(PathBuf, String, u64)> {
        let offer = self.with_doc(gid, |d| d.file(fid)).context("no such file")?;
        if offer.stopped {
            bail!("no longer shared");
        }
        let store = self.0.store.lock().unwrap();
        let path = store
            .state
            .shares
            .iter()
            .chain(&store.state.received)
            .find(|s| s.group == gid && s.file == fid)
            .map(|s| s.path.clone())
            .context("not on this device yet")?;
        Ok((path, offer.name, offer.size))
    }

    /// A guest finished downloading a file.
    pub(super) fn guest_downloaded(&self, gid: &str, fid: &str, guest: &str) {
        self.with_doc(gid, |d| d.mark_downloaded(fid, guest));
        self.doc_changed(gid, false);
    }

    /// Start receiving a file from a guest, if the link allows uploads.
    pub(super) async fn begin_guest_upload(&self, gid: &str, name: &str) -> Result<Upload> {
        if !self.guest_uploads(gid) {
            bail!("uploads are turned off");
        }
        let dir = self.download_dir().join("Guests");
        tokio::fs::create_dir_all(&dir).await?;
        let name = sharing::clean_file_name(name);
        let target = sharing::unique_path(&dir, &name);
        let part = target.with_extension("part");
        let file = tokio::fs::File::create(&part).await?;
        Ok(Upload { file, hasher: blake3::Hasher::new(), size: 0, part, target, name })
    }
}

/// A file a guest is uploading.
pub(super) struct Upload {
    file: tokio::fs::File,
    hasher: blake3::Hasher,
    size: u64,
    part: PathBuf,
    target: PathBuf,
    name: String,
}

impl Upload {
    pub(super) async fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.hasher.update(bytes);
        self.size += bytes.len() as u64;
        self.file.write_all(bytes).await
    }

    /// Done: the file becomes an offer in the group (from the guest, served by us).
    pub(super) async fn finish(mut self, core: &Core, gid: &str, origin: &Origin) -> Result<String> {
        self.file.flush().await?;
        tokio::fs::rename(&self.part, &self.target).await?;
        let hash = self.hasher.finalize().to_hex().to_string();
        Ok(core.add_guest_offer(gid, &origin.id(), &origin.name(), self.target.clone(), &self.name, self.size, &hash))
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        // Cancelled or failed halfway; after a successful `finish` the part no longer exists.
        let _ = std::fs::remove_file(&self.part);
    }
}

/// The IPv4 address the OS would reach the internet from: the one a browser on the same network can use.
fn lan_ip() -> Option<Ipv4Addr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}
