//! Keeps every group's document in sync over the network.
//!
//! Group members talk over the same control connections as paired peers
//! (`ControlMessage::GroupSync`). Members we are paired with already have a
//! connection, held by [`PairedConnectionManager`]; members we are not paired
//! with (a friend of a friend, invited by someone else in the group) are
//! dialed here, and are let in by the control endpoint for group traffic and
//! nothing else (`AccessState::group_peers`).
//!
//! An invitation is the group's document itself: the inviter adds us as a
//! pending member and syncs it to us, and we answer by changing our own entry.
//! A sync for a group we do not know is therefore only accepted from a paired
//! peer, who is the one that can invite us.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use data_encoding::BASE64;
use iroh::endpoint::Connection;
use iroh::EndpointId;
use tokio::sync::{Mutex, OnceCell, RwLock};
use tokio::task::JoinHandle;

use crate::native::device_identity::{DeviceIdentity, PairedDeviceStore};
use crate::native::group::{GroupDoc, Groups, MemberState, NewFile};
use crate::native::node::AccessState;
use crate::native::pairing_util::build_control_connect_addr;
use crate::native::runtime::NodeRuntime;
use crate::native::shares::ShareRegistry;
use crate::protocol::{
    read_message, write_message, AppHandle, ControlMessage, CONTROL_ALPN,
    PAIRED_RECONNECT_MAX_SECS, PAIRED_RECONNECT_MIN_SECS, PRESENCE_CONNECT_TIMEOUT_SECS,
};

/// How long to wait for a holder to answer a file request.
const FILE_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How long an offered download stays open if nobody finishes it.
const OFFER_LIFETIME: Duration = Duration::from_secs(6 * 60 * 60);

/// BLAKE3 of a file's content, in hex. Reads the whole file: call it off the
/// UI thread.
pub fn hash_file(path: &std::path::Path) -> anyhow::Result<String> {
    let file = std::fs::File::open(path).context("open file")?;
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(file).context("read file")?;
    Ok(hasher.finalize().to_hex().to_string())
}

/// How often group membership is re-read to dial new members and drop old ones.
const REFRESH_SECS: u64 = 30;

pub struct GroupService {
    groups: Groups,
    identity: Arc<DeviceIdentity>,
    paired_store: Arc<PairedDeviceStore>,
    access: Arc<RwLock<AccessState>>,
    app_handle: AppHandle,
    runtime: OnceCell<Arc<Mutex<NodeRuntime>>>,
    /// The newest connection to each member, however it came about.
    conns: std::sync::RwLock<HashMap<String, Connection>>,
    /// Dial loops for members we are not paired with.
    dialers: Mutex<HashMap<String, JoinHandle<()>>>,
    shares: Arc<ShareRegistry>,
    /// File requests waiting for their answer: a ticket, or why not.
    pending: std::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<Result<String, String>>>>,
    /// Held while a sync message is generated and written, so messages reach
    /// the peer in the order they were generated.
    send: Mutex<()>,
    shutdown: AtomicBool,
}

impl GroupService {
    pub(crate) fn new(
        data_dir: &std::path::Path,
        identity: Arc<DeviceIdentity>,
        paired_store: Arc<PairedDeviceStore>,
        access: Arc<RwLock<AccessState>>,
        app_handle: AppHandle,
        shares: Arc<ShareRegistry>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            groups: Groups::new(data_dir, &identity.endpoint_id())?,
            identity,
            paired_store,
            access,
            app_handle,
            runtime: OnceCell::new(),
            conns: std::sync::RwLock::new(HashMap::new()),
            dialers: Mutex::new(HashMap::new()),
            shares,
            pending: std::sync::Mutex::new(HashMap::new()),
            send: Mutex::new(()),
            shutdown: AtomicBool::new(false),
        })
    }

    pub fn groups(&self) -> &Groups {
        &self.groups
    }

    pub fn attach_runtime(&self, runtime: Arc<Mutex<NodeRuntime>>) {
        let _ = self.runtime.set(runtime);
    }

    /// Lets group members in and starts dialing them, then keeps doing so.
    pub fn start(self: &Arc<Self>) -> JoinHandle<()> {
        let service = Arc::clone(self);
        tokio::spawn(async move {
            while !service.shutdown.load(Ordering::SeqCst) {
                service.refresh().await;
                tokio::time::sleep(Duration::from_secs(REFRESH_SECS)).await;
            }
        })
    }

    pub async fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        for (_, handle) in self.dialers.lock().await.drain() {
            handle.abort();
        }
        self.conns.write().expect("conns lock").clear();
    }

    fn is_paired(&self, peer: &str) -> bool {
        self.paired_store
            .get(peer)
            .ok()
            .flatten()
            .is_some_and(|d| d.pairing_status.is_connectable())
    }

    fn emit_changed(&self) {
        if let Some(handle) = &self.app_handle {
            let _ = handle.emit_event_with_payload("groups-changed", "{}");
        }
    }

    /// Whether `peer` is, as far as we know, in a group with us.
    pub async fn is_group_peer(&self, peer: &EndpointId) -> bool {
        self.access.read().await.group_peers.contains(peer)
    }

    /// Re-reads who is in our groups: lets them in, dials the ones we are not
    /// paired with, and forgets the ones who are gone.
    pub async fn refresh(self: &Arc<Self>) {
        let peers: HashSet<String> = self
            .groups
            .list()
            .iter()
            .flat_map(|group| group.peers())
            .collect();
        self.access.write().await.group_peers = peers
            .iter()
            .filter_map(|peer| peer.parse::<EndpointId>().ok())
            .collect();

        let mut dialers = self.dialers.lock().await;
        dialers.retain(|peer, handle| {
            let keep = peers.contains(peer) && !self.is_paired(peer);
            if !keep {
                handle.abort();
            }
            keep
        });
        for peer in peers {
            if self.is_paired(&peer) || dialers.contains_key(&peer) {
                continue;
            }
            let handle = self.spawn_dialer(peer.clone());
            dialers.insert(peer, handle);
        }
    }

    /// Not `async`, so the dial loop's future (which ends up calling
    /// [`Self::refresh`] again) is not part of `refresh`'s own type.
    fn spawn_dialer(self: &Arc<Self>, peer: String) -> JoinHandle<()> {
        let service = Arc::clone(self);
        tokio::spawn(async move { service.dial_loop(peer).await })
    }

    // ---- connections ----

    /// A connection to `peer` is up: sync every group we share over it.
    pub async fn set_connection(self: &Arc<Self>, peer: &str, conn: Connection) {
        let peer = peer.to_lowercase();
        {
            let mut conns = self.conns.write().expect("conns lock");
            if conns.get(&peer).is_some_and(|current| {
                current.close_reason().is_none() && current.stable_id() == conn.stable_id()
            }) {
                return;
            }
            conns.insert(peer.clone(), conn.clone());
        }
        let service = Arc::clone(self);
        tokio::spawn(async move {
            for group in service.shared_with(&peer) {
                group.reset_sync(&peer);
                service.push(&group, &peer, &conn).await;
            }
        });
    }

    fn shared_with(&self, peer: &str) -> Vec<Arc<GroupDoc>> {
        self.groups
            .list()
            .into_iter()
            .filter(|group| group.peers().iter().any(|p| p == peer))
            .collect()
    }

    /// Whether we hold a working connection to `peer` right now.
    pub fn is_connected(&self, peer: &str) -> bool {
        self.conns
            .read()
            .expect("conns lock")
            .get(&peer.to_lowercase())
            .is_some_and(|conn| conn.close_reason().is_none())
    }

    async fn live_connection(&self, peer: &str) -> Option<Connection> {
        self.conns
            .read()
            .expect("conns lock")
            .get(peer)
            .filter(|conn| conn.close_reason().is_none())
            .cloned()
    }

    async fn connect(&self, peer: &str) -> anyhow::Result<Connection> {
        let remote: EndpointId = peer.parse().context("group member id")?;
        let runtime = self.runtime.get().context("group runtime not attached")?;
        let endpoint = runtime.lock().await.endpoint.clone();
        let addr = build_control_connect_addr(&endpoint, remote, None);
        tokio::time::timeout(
            Duration::from_secs(PRESENCE_CONNECT_TIMEOUT_SECS),
            endpoint.connect(addr, CONTROL_ALPN),
        )
        .await
        .context("group member connect timeout")?
        .context("group member connect failed")
    }

    async fn dial_loop(self: Arc<Self>, peer: String) {
        let mut backoff = PAIRED_RECONNECT_MIN_SECS;
        while !self.shutdown.load(Ordering::SeqCst) {
            if let Ok(conn) = self.connect(&peer).await {
                backoff = PAIRED_RECONNECT_MIN_SECS;
                self.set_connection(&peer, conn.clone()).await;
                tokio::select! {
                    _ = conn.closed() => {}
                    _ = self.read_loop(&peer, &conn) => {}
                }
            }
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(PAIRED_RECONNECT_MAX_SECS);
        }
    }

    /// Reads what a member sends on a connection we dialed.
    async fn read_loop(self: &Arc<Self>, peer: &str, conn: &Connection) {
        while let Ok((_send, mut recv)) = conn.accept_bi().await {
            if let Ok(message) = read_message(&mut recv).await {
                self.handle_message(peer, conn, message, self.is_paired(peer))
                    .await;
            }
        }
    }

    /// Handles a control message from `peer` if it is a group message.
    /// Returns false for any other message. The caller has checked that the
    /// peer may send group traffic at all (paired, or let in as a member).
    pub async fn handle_message(
        self: &Arc<Self>,
        peer: &str,
        conn: &Connection,
        message: ControlMessage,
        peer_is_paired: bool,
    ) -> bool {
        match message {
            ControlMessage::GroupSync { group_id, data } => {
                self.set_connection(peer, conn.clone()).await;
                self.receive(peer, &group_id, &data, conn, peer_is_paired)
                    .await;
                true
            }
            ControlMessage::GroupFileRequest {
                group_id,
                file_id,
                request_id,
            } => {
                self.spawn_serve(peer, conn, group_id, file_id, request_id);
                true
            }
            ControlMessage::GroupFileOffer {
                request_id,
                blob_ticket,
            } => {
                self.answer_request(&request_id, Ok(blob_ticket));
                true
            }
            ControlMessage::GroupFileDenied { request_id, reason } => {
                self.answer_request(&request_id, Err(reason));
                true
            }
            _ => false,
        }
    }

    // ---- files ----

    fn answer_request(&self, request_id: &str, answer: Result<String, String>) {
        if let Some(waiting) = self.pending.lock().expect("pending lock").remove(request_id) {
            let _ = waiting.send(answer);
        }
    }

    /// Not `async`, for the same reason as [`Self::spawn_dialer`].
    fn spawn_serve(
        self: &Arc<Self>,
        peer: &str,
        conn: &Connection,
        group_id: String,
        file_id: String,
        request_id: String,
    ) {
        let service = Arc::clone(self);
        let (peer, conn) = (peer.to_lowercase(), conn.clone());
        tokio::spawn(async move {
            let reply = match service.offer(&peer, &group_id, &file_id).await {
                Ok(blob_ticket) => ControlMessage::GroupFileOffer {
                    request_id,
                    blob_ticket,
                },
                Err(err) => ControlMessage::GroupFileDenied {
                    request_id,
                    reason: format!("{err:#}"),
                },
            };
            let sent = async {
                let (mut send, _recv) = conn.open_bi().await?;
                write_message(&mut send, &reply).await
            }
            .await;
            if let Err(err) = sent {
                tracing::debug!("answering {peer}'s file request failed: {err:#}");
            }
        });
    }

    /// Opens a download of a file we hold, for a member who asked.
    async fn offer(&self, peer: &str, group_id: &str, file_id: &str) -> anyhow::Result<String> {
        let group = self.groups.get(group_id).context("I am not in that group")?;
        anyhow::ensure!(
            group
                .members()
                .iter()
                .any(|m| m.endpoint_id == peer && m.state == MemberState::Active),
            "you are not in that group"
        );
        let file = group
            .files()
            .into_iter()
            .find(|f| f.id == file_id)
            .context("that file is not listed any more")?;
        let path = file.local_path.context("I do not have that file")?;
        let size = tokio::fs::metadata(&path).await.context("file is gone")?.len();
        anyhow::ensure!(size == file.size, "my copy has changed");
        let remote: EndpointId = peer.parse().context("member id")?;
        let runtime = self.runtime.get().context("not ready")?;
        let addr = runtime.lock().await.endpoint.addr();
        let share = self.shares.share(addr, remote, vec![path], None).await?;
        let ticket = share.ticket.clone();
        // Keep serving until it has been fetched, or nobody comes for it.
        tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + OFFER_LIFETIME;
            while share.completed_peers() == 0 && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            drop(share);
        });
        Ok(ticket)
    }

    /// Asks `peer` for a file, and returns the ticket to download it with.
    async fn ask(&self, peer: &str, group_id: &str, file_id: &str) -> anyhow::Result<String> {
        let conn = self.live_connection(peer).await.context("not online")?;
        let request_id = uuid::Uuid::new_v4().simple().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending
            .lock()
            .expect("pending lock")
            .insert(request_id.clone(), tx);
        let result = async {
            let (mut send, _recv) = conn.open_bi().await?;
            let request = ControlMessage::GroupFileRequest {
                group_id: group_id.to_string(),
                file_id: file_id.to_string(),
                request_id: request_id.clone(),
            };
            write_message(&mut send, &request).await?;
            let answer = tokio::time::timeout(FILE_REQUEST_TIMEOUT, rx)
                .await
                .context("no answer")?
                .context("no answer")?;
            answer.map_err(|reason| anyhow::anyhow!(reason))
        }
        .await;
        self.pending.lock().expect("pending lock").remove(&request_id);
        result
    }

    /// Asks the online members who hold a file for it, one at a time, until
    /// one agrees. Returns who, and the ticket to download it with.
    pub async fn request_file(
        &self,
        group_id: &str,
        file_id: &str,
    ) -> anyhow::Result<(String, String)> {
        let group = self.groups.get(group_id).context("no such group")?;
        let file = group
            .files()
            .into_iter()
            .find(|f| f.id == file_id)
            .context("that file is not listed any more")?;
        let me = self.identity.endpoint_id().to_lowercase();
        let mut holders: Vec<String> = file
            .holders
            .into_iter()
            .filter(|holder| *holder != me && self.is_connected(holder))
            .collect();
        anyhow::ensure!(!holders.is_empty(), "nobody who has it is online");
        // Spread the load: a different holder first each time.
        if let Ok(start) = getrandom::u32() {
            let len = holders.len();
            holders.rotate_left(start as usize % len);
        }
        let mut last = None;
        for holder in holders {
            match self.ask(&holder, group_id, file_id).await {
                Ok(ticket) => return Ok((holder, ticket)),
                Err(err) => {
                    tracing::debug!("{holder} would not send the file: {err:#}");
                    last = Some(err);
                }
            }
        }
        Err(last.expect("at least one holder"))
    }

    /// Lists a file of ours in the group.
    pub async fn share_file(
        self: &Arc<Self>,
        group_id: &str,
        path: std::path::PathBuf,
    ) -> anyhow::Result<()> {
        let group = self.groups.get(group_id).context("no such group")?;
        let file = tokio::task::spawn_blocking(move || {
            anyhow::ensure!(path.is_file(), "only files can be shared in a group");
            let size = std::fs::metadata(&path)?.len();
            let hash = hash_file(&path)?;
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "file".to_string());
            Ok(NewFile {
                name,
                size,
                hash,
                path,
            })
        })
        .await??;
        group.add_file(&file)?;
        self.changed(group_id).await;
        Ok(())
    }

    /// We downloaded a file the group lists, and can serve it now too.
    pub async fn record_downloaded(
        self: &Arc<Self>,
        group_id: &str,
        file_id: &str,
        path: &std::path::Path,
    ) -> anyhow::Result<()> {
        let group = self.groups.get(group_id).context("no such group")?;
        group.set_local_path(file_id, path)?;
        group.add_holder(file_id)?;
        self.changed(group_id).await;
        Ok(())
    }

    /// Stops listing a file we own, or stops serving our copy of someone's.
    pub async fn remove_file(
        self: &Arc<Self>,
        group_id: &str,
        file_id: &str,
    ) -> anyhow::Result<()> {
        let group = self.groups.get(group_id).context("no such group")?;
        let owned = group
            .files()
            .into_iter()
            .find(|f| f.id == file_id)
            .is_some_and(|f| f.owner == self.identity.endpoint_id().to_lowercase());
        if owned {
            group.remove_file(file_id)?;
        } else {
            group.remove_holder(file_id)?;
        }
        self.changed(group_id).await;
        Ok(())
    }

    // ---- sync ----

    async fn push(&self, group: &GroupDoc, peer: &str, conn: &Connection) {
        let _ordered = self.send.lock().await;
        let Some(message) = group.generate_sync(peer) else {
            return;
        };
        let data = BASE64.encode(&message);
        let group_id = group.id().to_string();
        let sent = async {
            let (mut send, _recv) = conn.open_bi().await?;
            write_message(&mut send, &ControlMessage::GroupSync { group_id, data }).await
        }
        .await;
        if let Err(err) = sent {
            tracing::debug!("group sync to {peer} failed: {err:#}");
            // That message is lost: sync from scratch when it comes back.
            group.reset_sync(peer);
        }
    }

    /// Sends every online member of the group whatever they are missing.
    pub async fn push_group(&self, group_id: &str) {
        let Some(group) = self.groups.get(group_id) else {
            return;
        };
        for peer in group.peers() {
            if let Some(conn) = self.live_connection(&peer).await {
                self.push(&group, &peer, &conn).await;
            }
        }
    }

    async fn receive(
        self: &Arc<Self>,
        peer: &str,
        group_id: &str,
        data: &str,
        conn: &Connection,
        peer_is_paired: bool,
    ) {
        let peer = peer.to_lowercase();
        let Ok(bytes) = BASE64.decode(data.as_bytes()) else {
            tracing::warn!("group sync from {peer} is not base64");
            return;
        };
        let group = match self.groups.get(group_id) {
            Some(group) => {
                let members = group.members();
                let known = members
                    .iter()
                    .any(|m| m.endpoint_id == peer && m.state.is_connectable());
                // Before the document arrives the only thing we know is who
                // sent it, and that has to be a paired peer.
                if !(known || members.is_empty() && peer_is_paired) {
                    tracing::debug!("group sync from {peer}, who is not in that group");
                    return;
                }
                group
            }
            // Someone starting a group with us, or inviting us to theirs.
            None if peer_is_paired => match self.groups.join(group_id) {
                Ok(group) => group,
                Err(err) => {
                    tracing::warn!("bad group sync from {peer}: {err:#}");
                    return;
                }
            },
            None => return,
        };
        match group.receive_sync(&peer, &bytes) {
            Ok(true) => {
                self.refresh().await;
                self.emit_changed();
            }
            Ok(false) => {}
            Err(err) => {
                tracing::warn!("bad group sync from {peer}: {err:#}");
                return;
            }
        }
        self.push(&group, &peer, conn).await;
    }

    // ---- what the user does ----

    /// Something in the group changed here: tell everyone and the UI.
    pub async fn changed(self: &Arc<Self>, group_id: &str) {
        self.refresh().await;
        self.emit_changed();
        self.push_group(group_id).await;
    }

    fn paired_name(&self, peer: &str) -> anyhow::Result<String> {
        let device = self
            .paired_store
            .get(peer)?
            .filter(|d| d.pairing_status.is_connectable())
            .context("not a paired peer")?;
        Ok(device.display_name)
    }

    /// Starts a group with an approved peer, who is invited to it.
    pub async fn create_group(self: &Arc<Self>, peer: &str) -> anyhow::Result<String> {
        let name = self.paired_name(peer)?;
        let group = self
            .groups
            .create(&self.identity.display_name(), peer, &name)?;
        self.changed(group.id()).await;
        Ok(group.id().to_string())
    }

    /// Invites one of our approved peers to a group we are in.
    pub async fn invite(self: &Arc<Self>, group_id: &str, peer: &str) -> anyhow::Result<()> {
        let name = self.paired_name(peer)?;
        let group = self.groups.get(group_id).context("no such group")?;
        group.invite(peer, &name)?;
        self.changed(group_id).await;
        Ok(())
    }

    /// Accepts or declines an invitation we received.
    pub async fn answer_invite(
        self: &Arc<Self>,
        group_id: &str,
        accept: bool,
    ) -> anyhow::Result<()> {
        let group = self.groups.get(group_id).context("no such group")?;
        group.answer_invite(accept, &self.identity.display_name())?;
        self.changed(group_id).await;
        Ok(())
    }

    /// Leaves a group, once the others have been told.
    pub async fn leave(self: &Arc<Self>, group_id: &str) -> anyhow::Result<()> {
        let group = self.groups.get(group_id).context("no such group")?;
        anyhow::ensure!(
            group.my_state() == Some(MemberState::Active),
            "not in this group"
        );
        // Tell them while we still know who they are.
        let peers = group.peers();
        group.leave()?;
        for peer in peers {
            if let Some(conn) = self.live_connection(&peer).await {
                self.push(&group, &peer, &conn).await;
            }
        }
        self.emit_changed();
        Ok(())
    }
}
