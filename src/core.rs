//! Networking core: one iroh endpoint, LAN discovery (mDNS), peer bookkeeping, saved contacts that
//! reconnect automatically, group invites and a tiny hello/chat protocol. UI-agnostic; the UI
//! subscribes through the event callback.

mod sharing;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use iroh::{
    Endpoint, EndpointAddr, EndpointId,
    address_lookup::MemoryLookup,
    endpoint::{Connection, presets},
    endpoint_info::UserData,
};
use iroh_ble_transport::BleTransport;
use iroh_mdns_address_lookup::{DiscoveryEvent, MdnsAddressLookup};
use iroh_tickets::endpoint::EndpointTicket;
use n0_future::StreamExt;
use n0_watcher::Watcher;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::{
    settings::{Settings, clean_name},
    state::{Contact, Group, Invite, Member, Reply, State, Store},
};

pub use crate::state::MemberState;
pub use sharing::{DocMember, FileView, GroupView, TextView};

pub const ALPN: &[u8] = b"flipflop/0";
const MDNS_SERVICE: &str = "flipflop";
/// Control messages are JSON and small; sync messages (first byte [`sharing::SYNC_TAG`]) can carry a whole document.
const MAX_MSG_BYTES: usize = 64 * 1024;
const MAX_SYNC_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct Peer {
    pub id: EndpointId,
    /// The name the peer itself uses (or a short id if we have never heard one).
    pub name: String,
    /// Our local rename, if any (saved contacts only).
    pub alias: Option<String>,
    /// In the contact list: reconnects automatically.
    pub saved: bool,
    pub lan: bool,
    pub ble: bool,
    pub connected: bool,
    /// We paired with them.
    pub paired: bool,
    /// They paired with us.
    pub paired_us: bool,
    /// Both paired: the implied group of the two of us.
    pub pair_group: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemberInfo {
    pub id: EndpointId,
    /// Display name (alias, else the peer's own name).
    pub name: String,
    pub state: MemberState,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GroupInfo {
    pub id: String,
    pub haiku: String,
    pub alias: Option<String>,
    pub members: Vec<MemberInfo>,
    /// Contacts that could still be added.
    pub candidates: Vec<(EndpointId, String)>,
    pub auto_download: bool,
    pub auto_stop: bool,
    /// An implied group (see [`Core::pair`]): the other device. Not listed with the groups.
    pub with: Option<EndpointId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InviteInfo {
    pub inviter: EndpointId,
    pub inviter_name: String,
    pub group: String,
    pub haiku: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct GroupsView {
    pub groups: Vec<GroupInfo>,
    pub invites: Vec<InviteInfo>,
}

#[derive(Debug, Clone)]
pub enum Event {
    Peers(Vec<Peer>),
    Groups(GroupsView),
    /// Our own shareable ticket changed (new addresses, relay came up…).
    Ticket(String),
    /// Our own name changed (also fired once at start-up).
    Name(String),
    Message { peer: EndpointId, mine: bool, text: String },
    /// The shared content of a group changed (files, texts, scratchpad, members, transfer progress).
    Group(GroupView),
    Notice(String),
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Msg {
    Hello { name: String },
    Text { text: String },
    /// "Please join this group" (the haiku name derives from the id, so only the id travels).
    GroupInvite { group: String },
    GroupReply { group: String, accept: bool },
    /// "I paired with you" / "I no longer pair with you".
    Pair,
    Unpair,
}

/// What a BLE peripheral serves and a BLE central reads: our name and ticket.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BlePayload {
    pub n: String,
    pub t: String,
}

#[derive(Default)]
struct PeerEntry {
    announced_name: Option<String>, // from mDNS / BLE
    hello_name: Option<String>,     // from the connection handshake
    lan: bool,
    ble: bool,   // seen by our GATT name+ticket discovery
    ble_t: bool, // known to the BLE transport (iroh can reach it over Bluetooth)
    conn: Option<Connection>,
}

struct Inner {
    /// Exclusive lock on the data dir, held for the life of the node.
    dir_lock: std::fs::File,
    endpoint: Endpoint,
    ble_transport: Option<Arc<BleTransport>>,
    ble_transport_status: String,
    memory: MemoryLookup,
    settings: Mutex<Settings>,
    store: Mutex<Store>,
    /// Reconnect backoff per contact: (not before, current delay). Absent = try now.
    retry: Mutex<HashMap<EndpointId, (Instant, Duration)>>,
    dialing: Mutex<HashSet<EndpointId>>,
    /// Held while snapshotting and emitting, so the UI never sees an older view after a newer one.
    publish_lock: Mutex<()>,
    peers: Mutex<BTreeMap<EndpointId, PeerEntry>>,
    sharing: sharing::Sharing,
    ticket: Mutex<String>,
    ble_payload: watch::Sender<Option<BlePayload>>,
    on_event: Box<dyn Fn(Event) + Send + Sync>,
}

#[derive(Clone)]
pub struct Core(Arc<Inner>);

impl Core {
    /// Is another process running a node on this data dir?
    pub fn dir_in_use(data_dir: &Path) -> bool {
        match std::fs::File::create(data_dir.join("lock")) {
            Ok(f) => matches!(f.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
            Err(_) => false,
        }
    }

    /// Must be called from within a tokio runtime. With `ble_transport`, iroh also gets Bluetooth LE
    /// as a network path (custom transports can only be added at bind time, so on Android the
    /// Bluetooth permission must already be granted).
    pub async fn start(
        data_dir: &Path,
        ble_transport: bool,
        on_event: impl Fn(Event) + Send + Sync + 'static,
    ) -> Result<Core> {
        std::fs::create_dir_all(data_dir).with_context(|| format!("creating {}", data_dir.display()))?;
        let dir_lock = std::fs::File::create(data_dir.join("lock")).context("opening lock file")?;
        dir_lock.try_lock().map_err(|_| {
            anyhow!("{} is in use by another flipflop (GUI or CLI); stop it, or set FLIPFLOP_DIR for a separate identity", data_dir.display())
        })?;
        let settings = Settings::load(data_dir);
        let secret_key = settings.secret_key.clone();
        let name = settings.name.clone();

        let mdns = MdnsAddressLookup::builder()
            .service_name(MDNS_SERVICE)
            .build(secret_key.public())
            .context("starting mDNS")?;
        // Subscribe before anything can be discovered: already-known peers are not re-announced.
        let lan_events = mdns.subscribe().await;
        let memory = MemoryLookup::new();

        let (ble, ble_transport_status) = if ble_transport {
            match BleTransport::builder().build(secret_key.public()).await {
                Ok(ble) => (Some(ble), "on".to_string()),
                Err(err) => {
                    tracing::warn!("BLE transport unavailable: {err:?}");
                    (None, format!("unavailable ({err})"))
                }
            }
        } else {
            (None, "off".to_string())
        };

        let mut builder = Endpoint::builder(presets::N0)
            .secret_key(secret_key)
            .alpns(vec![ALPN.to_vec()])
            .address_lookup(mdns.clone())
            .address_lookup(memory.clone())
            .user_data_for_address_lookup(user_data(&name));
        if let Some(ble) = &ble {
            builder = builder
                .hooks(ble.dedup_hook())
                .add_custom_transport(ble.as_custom_transport())
                .address_lookup(ble.address_lookup());
        }
        let endpoint = builder.bind().await.context("binding iroh endpoint")?;

        let core = Core(Arc::new(Inner {
            dir_lock: dir_lock,
            endpoint,
            ble_transport: ble,
            ble_transport_status,
            memory,
            settings: Mutex::new(settings),
            store: Mutex::new(Store::load(data_dir)),
            retry: Mutex::default(),
            dialing: Mutex::default(),
            publish_lock: Mutex::default(),
            peers: Mutex::default(),
            sharing: sharing::Sharing::new(data_dir),
            ticket: Mutex::default(),
            ble_payload: watch::channel(None).0,
            on_event: Box::new(on_event),
        }));
        core.emit(Event::Name(name));
        core.load_docs();
        core.publish();
        core.spawn_background(lan_events);
        Ok(core)
    }

    fn emit(&self, event: Event) {
        (self.0.on_event)(event);
    }

    /// `on`, `off` or `unavailable (reason)`: whether iroh can use Bluetooth as a path.
    pub fn ble_transport_status(&self) -> &str {
        &self.0.ble_transport_status
    }

    pub fn id(&self) -> EndpointId {
        self.0.endpoint.id()
    }

    pub fn name(&self) -> String {
        self.0.settings.lock().unwrap().name.clone()
    }

    pub fn ticket(&self) -> String {
        self.0.ticket.lock().unwrap().clone()
    }

    /// Payload for BLE advertising; `None` until the first ticket is known.
    pub fn ble_payload(&self) -> watch::Receiver<Option<BlePayload>> {
        self.0.ble_payload.subscribe()
    }

    pub fn set_name(&self, name: &str) {
        let name = self.0.settings.lock().unwrap().set_name(name);
        self.0.endpoint.set_user_data_for_address_lookup(Some(user_data(&name)));
        self.refresh_ble_payload();
        self.emit(Event::Name(name.clone()));
        self.refresh_doc_names();
        let this = self.clone();
        let conns: Vec<Connection> =
            self.0.peers.lock().unwrap().values().filter_map(|p| p.conn.clone()).collect();
        tokio::spawn(async move {
            for conn in conns {
                let _ = send_msg(&conn, &Msg::Hello { name: name.clone() }).await;
            }
            drop(this);
        });
    }

    fn refresh_ble_payload(&self) {
        let ticket = self.ticket();
        let payload = (!ticket.is_empty()).then(|| BlePayload { n: self.name(), t: ticket });
        self.0.ble_payload.send_replace(payload);
    }

    // ---- peers ------------------------------------------------------------------------

    /// Run `f` on the persistent state, save it and tell the UI.
    fn with_state<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        let result = {
            let mut store = self.0.store.lock().unwrap();
            let result = f(&mut store.state);
            store.save();
            result
        };
        self.publish();
        result
    }

    /// Names peers use for themselves right now (handshake, else announcement).
    fn live_names(&self) -> BTreeMap<EndpointId, String> {
        let peers = self.0.peers.lock().unwrap();
        peers.iter().filter_map(|(id, p)| Some((*id, p.hello_name.clone().or_else(|| p.announced_name.clone())?))).collect()
    }

    /// Push the peer list and the groups view to the UI.
    fn publish(&self) {
        let _ordered = self.0.publish_lock.lock().unwrap();
        let (peers, groups) = self.snapshot();
        self.emit(Event::Peers(peers));
        self.emit(Event::Groups(groups));
        self.refresh_group_views();
    }

    pub fn peers(&self) -> Vec<Peer> {
        self.snapshot().0
    }

    pub fn groups(&self) -> GroupsView {
        self.snapshot().1
    }

    fn snapshot(&self) -> (Vec<Peer>, GroupsView) {
        let state = {
            let store = self.0.store.lock().unwrap();
            State {
                contacts: store.state.contacts.clone(),
                groups: store.state.groups.clone(),
                invites: store.state.invites.clone(),
                rejected: Vec::new(),
                replies: Vec::new(),
                forgotten: Vec::new(),
                paired_us: store.state.paired_us.clone(),
                shares: Vec::new(),
                received: Vec::new(),
            }
        };
        let live = self.live_names();
        let contact = |id: &EndpointId| state.contacts.iter().find(|c| c.id == id.to_string());
        let remote_name = |id: &EndpointId| {
            live.get(id)
                .cloned()
                .or_else(|| contact(id).and_then(|c| c.name.clone()))
                .unwrap_or_else(|| id.fmt_short().to_string())
        };
        let display_name = |id: &EndpointId| contact(id).and_then(|c| c.alias.clone()).unwrap_or_else(|| remote_name(id));
        let pairing = |id: &EndpointId| {
            let paired = contact(id).is_some_and(|c| c.paired);
            let paired_us = state.paired_us.contains(&id.to_string());
            let group = (paired && paired_us).then(|| pair_group_id(&self.id(), id));
            (paired, paired_us, group)
        };

        let mut list: Vec<Peer> = {
            let peers = self.0.peers.lock().unwrap();
            let mut list: Vec<Peer> = peers
                .iter()
                .map(|(id, p)| {
                    let (paired, paired_us, pair_group) = pairing(id);
                    Peer {
                        id: *id,
                        name: remote_name(id),
                        alias: contact(id).and_then(|c| c.alias.clone()),
                        saved: contact(id).is_some(),
                        lan: p.lan,
                        ble: p.ble || p.ble_t,
                        connected: p.conn.is_some(),
                        paired,
                        paired_us,
                        pair_group,
                    }
                })
                .collect();
            // Saved contacts stay listed while offline.
            for c in &state.contacts {
                let Ok(id) = c.id.parse::<EndpointId>() else { continue };
                if !peers.contains_key(&id) {
                    let (paired, paired_us, pair_group) = pairing(&id);
                    list.push(Peer { id, name: remote_name(&id), alias: c.alias.clone(), saved: true, lan: false, ble: false, connected: false, paired, paired_us, pair_group });
                }
            }
            list
        };
        list.sort_by(|a, b| {
            b.connected
                .cmp(&a.connected)
                .then_with(|| b.saved.cmp(&a.saved))
                .then_with(|| a.alias.as_ref().unwrap_or(&a.name).cmp(b.alias.as_ref().unwrap_or(&b.name)))
        });

        let groups = state
            .groups
            .iter()
            .filter_map(|g| {
                let haiku = crate::haiku::name(&parse_gid(&g.id)?);
                let members: Vec<MemberInfo> = g
                    .members
                    .iter()
                    .filter_map(|m| {
                        let id = m.peer.parse::<EndpointId>().ok()?;
                        Some(MemberInfo { id, name: display_name(&id), state: m.state })
                    })
                    .collect();
                let candidates = state
                    .contacts
                    .iter()
                    .filter_map(|c| c.id.parse::<EndpointId>().ok())
                    .filter(|id| !members.iter().any(|m| m.id == *id))
                    .map(|id| (id, display_name(&id)))
                    .collect();
                Some(GroupInfo {
                    id: g.id.clone(),
                    haiku,
                    alias: g.alias.clone(),
                    members,
                    candidates,
                    auto_download: g.auto_download,
                    auto_stop: g.auto_stop,
                    with: g.with.as_ref().and_then(|w| w.parse().ok()),
                })
            })
            .collect();
        let invites = state
            .invites
            .iter()
            .filter_map(|i| {
                let inviter = i.inviter.parse::<EndpointId>().ok()?;
                Some(InviteInfo { inviter, inviter_name: display_name(&inviter), group: i.group.clone(), haiku: crate::haiku::name(&parse_gid(&i.group)?) })
            })
            .collect();
        (list, GroupsView { groups, invites })
    }

    fn publish_peers(&self) {
        self.publish();
    }

    /// Update a peer entry and republish the list. Dead entries (not connected, no source) are removed.
    fn update_peer(&self, id: EndpointId, f: impl FnOnce(&mut PeerEntry)) {
        if id == self.id() {
            return;
        }
        let appeared = {
            let mut peers = self.0.peers.lock().unwrap();
            let entry = peers.entry(id).or_default();
            let was_reachable = entry.lan || entry.ble || entry.ble_t;
            f(entry);
            let reachable = entry.lan || entry.ble || entry.ble_t;
            let appeared = reachable && !was_reachable;
            if !reachable && entry.conn.is_none() {
                peers.remove(&id);
            }
            appeared
        };
        if appeared {
            // A saved contact just showed up nearby: don't wait out its reconnect backoff.
            self.0.retry.lock().unwrap().remove(&id);
        }
        self.publish();
    }

    /// A peer seen over BLE: remember how to reach it (address from its ticket) and its name.
    pub fn ble_seen(&self, payload: BlePayload) {
        let Ok(ticket) = payload.t.parse::<EndpointTicket>() else { return };
        let addr = ticket.endpoint_addr().clone();
        let id = addr.id;
        if id == self.id() {
            return;
        }
        self.0.memory.add_endpoint_info(addr);
        let name = crate::settings::clean_name(&payload.n);
        self.update_peer(id, |p| {
            p.ble = true;
            p.announced_name = Some(name);
        });
    }

    pub fn ble_gone(&self, id: EndpointId) {
        self.update_peer(id, |p| p.ble = false);
    }

    // ---- connections ------------------------------------------------------------------

    /// Connect using a pasted / scanned ticket.
    pub async fn connect_ticket(&self, text: &str) -> Result<EndpointId> {
        let ticket: EndpointTicket =
            text.trim().parse().map_err(|e| anyhow!("not a flipflop ticket: {e}"))?;
        let addr: EndpointAddr = ticket.into();
        let id = addr.id;
        if id == self.id() {
            return Err(anyhow!("that is this device's own ticket"));
        }
        self.0.memory.add_endpoint_info(addr);
        self.update_peer(id, |_| {});
        self.connect(id).await?;
        // Scanning or pasting someone's code is choosing to pair with them.
        self.pair(id);
        Ok(id)
    }

    pub async fn connect(&self, id: EndpointId) -> Result<()> {
        if self.0.peers.lock().unwrap().get(&id).is_some_and(|p| p.conn.is_some()) {
            // Already connected (they dialed us, say): asking to connect saves them.
            self.remember(id);
            return Ok(());
        }
        let conn = self
            .0
            .endpoint
            .connect(id, ALPN)
            .await
            .map_err(|e| anyhow!("could not connect: {e}"))?;
        self.adopt(conn, true);
        Ok(())
    }

    pub fn disconnect(&self, id: EndpointId) {
        let conn = self.0.peers.lock().unwrap().get(&id).and_then(|p| p.conn.clone());
        if let Some(conn) = conn {
            conn.close(0u32.into(), b"bye");
        }
    }

    pub async fn send(&self, id: EndpointId, text: String) -> Result<()> {
        let conn = self
            .0
            .peers
            .lock()
            .unwrap()
            .get(&id)
            .and_then(|p| p.conn.clone())
            .context("not connected")?;
        send_msg(&conn, &Msg::Text { text: text.clone() }).await?;
        self.emit(Event::Message { peer: id, mine: true, text });
        Ok(())
    }

    /// Register a live connection (incoming or outgoing) and start serving it.
    fn adopt(&self, conn: Connection, outgoing: bool) {
        let id = conn.remote_id();
        let stable = conn.stable_id();
        // Both sides may dial each other at once. They must agree on which connection survives:
        // the one dialed by the endpoint with the smaller id.
        let new_wins = if outgoing { self.id() < id } else { id < self.id() };
        let previous = {
            let mut peers = self.0.peers.lock().unwrap();
            let entry = peers.entry(id).or_default();
            if !new_wins && entry.conn.as_ref().is_some_and(|c| c.close_reason().is_none()) {
                conn.close(0u32.into(), b"duplicate");
                return;
            }
            entry.conn.replace(conn.clone())
        };
        if let Some(old) = previous {
            old.close(0u32.into(), b"replaced");
        }
        // Connections we dial are saved; so are incoming ones, unless the user deleted that contact.
        if outgoing || !self.is_forgotten(id) {
            self.remember(id);
        }
        self.0.retry.lock().unwrap().remove(&id);
        self.reset_doc_sync(id);
        self.publish_peers();
        self.emit(Event::Notice(format!("connected to {}", id.fmt_short())));

        // File downloads: the other side opens a bidirectional stream per request.
        let this = self.clone();
        let serving = conn.clone();
        tokio::spawn(async move {
            while let Ok((tx, rx)) = serving.accept_bi().await {
                let this = this.clone();
                tokio::spawn(async move { this.serve_download(id, tx, rx).await });
            }
        });

        let this = self.clone();
        tokio::spawn(async move {
            let hello = Msg::Hello { name: this.name() };
            if let Err(err) = send_msg(&conn, &hello).await {
                tracing::debug!("hello failed: {err:#}");
            }
            // Our side of the pairing, so a change made while they were away still reaches them.
            let _ = send_msg(&conn, &if this.is_paired(id) { Msg::Pair } else { Msg::Unpair }).await;
            this.flush(id).await;
            while let Ok(mut rx) = conn.accept_uni().await {
                let Ok(bytes) = rx.read_to_end(MAX_SYNC_BYTES).await else { continue };
                if bytes.first() == Some(&sharing::SYNC_TAG) {
                    this.on_sync(id, &bytes[1..]);
                    continue;
                }
                if bytes.len() > MAX_MSG_BYTES {
                    continue;
                }
                match serde_json::from_slice::<Msg>(&bytes) {
                    Ok(Msg::Hello { name }) => {
                        let name = clean_name(&name);
                        this.update_peer(id, |p| p.hello_name = Some(name.clone()));
                        this.with_state(|st| {
                            if let Some(c) = st.contacts.iter_mut().find(|c| c.id == id.to_string()) {
                                c.name = Some(name);
                            }
                        });
                    }
                    Ok(Msg::Text { text }) => {
                        this.emit(Event::Message { peer: id, mine: false, text });
                    }
                    Ok(Msg::GroupInvite { group }) => this.on_invite(id, group),
                    Ok(Msg::GroupReply { group, accept }) => this.on_reply(id, group, accept),
                    Ok(Msg::Pair) => this.on_pair(id, true),
                    Ok(Msg::Unpair) => this.on_pair(id, false),
                    Err(err) => tracing::debug!("bad message: {err}"),
                }
            }
            // Only clear the slot if it still holds *this* connection (not a replacement).
            let mut gone = false;
            this.update_peer(id, |p| {
                if p.conn.as_ref().is_some_and(|c| c.stable_id() == stable) {
                    p.conn = None;
                    p.hello_name = None;
                    gone = true;
                }
            });
            if gone {
                this.emit(Event::Notice(format!("disconnected from {}", id.fmt_short())));
            }
        });
    }

    // ---- contacts ---------------------------------------------------------------------

    /// Every peer we connect to goes in the contact list (and is redialed from now on).
    fn remember(&self, id: EndpointId) {
        let name = self.0.peers.lock().unwrap().get(&id).and_then(|p| p.hello_name.clone().or_else(|| p.announced_name.clone()));
        self.with_state(|st| {
            let key = id.to_string();
            st.forgotten.retain(|f| *f != key);
            match st.contacts.iter_mut().find(|c| c.id == key) {
                Some(c) => {
                    if name.is_some() {
                        c.name = name;
                    }
                }
                None => st.contacts.push(Contact { id: key, name, alias: None, paired: false }),
            }
        });
    }

    /// Local rename; an empty alias goes back to the peer's own name.
    pub fn rename_contact(&self, id: EndpointId, alias: &str) {
        let alias = Some(clean_name(alias)).filter(|a| !a.is_empty());
        self.with_state(|st| {
            if let Some(c) = st.contacts.iter_mut().find(|c| c.id == id.to_string()) {
                c.alias = alias;
            }
        });
    }

    /// Remove from the contact list: no more automatic reconnects, and they are not saved again
    /// just because they connect to us.
    pub fn forget_contact(&self, id: EndpointId) {
        self.0.retry.lock().unwrap().remove(&id);
        self.with_state(|st| {
            let key = id.to_string();
            st.contacts.retain(|c| c.id != key);
            if !st.forgotten.contains(&key) {
                st.forgotten.push(key);
            }
        });
        self.tell(id, Msg::Unpair);
        self.sync_pair_group(id);
    }

    fn is_forgotten(&self, id: EndpointId) -> bool {
        self.0.store.lock().unwrap().state.forgotten.contains(&id.to_string())
    }

    // ---- pairing ----------------------------------------------------------------------

    /// Pair with a peer: we trust it, and tell it so. Once it has paired with us as well, the two
    /// of us share an implied group.
    pub fn pair(&self, id: EndpointId) {
        self.remember_unconnected(id);
        self.with_state(|st| {
            if let Some(c) = st.contacts.iter_mut().find(|c| c.id == id.to_string()) {
                c.paired = true;
            }
        });
        self.tell(id, Msg::Pair);
        self.sync_pair_group(id);
    }

    /// Stop trusting a peer. The implied group goes away on both sides.
    pub fn unpair(&self, id: EndpointId) {
        self.with_state(|st| {
            if let Some(c) = st.contacts.iter_mut().find(|c| c.id == id.to_string()) {
                c.paired = false;
            }
        });
        self.tell(id, Msg::Unpair);
        self.sync_pair_group(id);
    }

    fn is_paired(&self, id: EndpointId) -> bool {
        self.0.store.lock().unwrap().state.contacts.iter().any(|c| c.id == id.to_string() && c.paired)
    }

    fn on_pair(&self, from: EndpointId, paired: bool) {
        let key = from.to_string();
        let known = self.0.store.lock().unwrap().state.paired_us.contains(&key);
        if known != paired {
            self.with_state(|st| {
                st.paired_us.retain(|p| *p != key);
                if paired {
                    st.paired_us.push(key);
                }
            });
        }
        self.sync_pair_group(from);
    }

    /// Send a message to a peer if it is connected (pairing is re-announced on every connection).
    fn tell(&self, id: EndpointId, msg: Msg) {
        let Some(conn) = self.0.peers.lock().unwrap().get(&id).and_then(|p| p.conn.clone()) else { return };
        tokio::spawn(async move {
            let _ = send_msg(&conn, &msg).await;
        });
    }

    /// The implied group exists exactly while both of us have paired.
    fn sync_pair_group(&self, id: EndpointId) {
        let key = id.to_string();
        let gid = pair_group_id(&self.id(), &id);
        let (mutual, exists) = {
            let store = self.0.store.lock().unwrap();
            let st = &store.state;
            (st.contacts.iter().any(|c| c.id == key && c.paired) && st.paired_us.contains(&key), st.groups.iter().any(|g| g.id == gid))
        };
        match (mutual, exists) {
            (true, false) => {
                self.with_state(|st| {
                    let mut g = Group::new(gid.clone(), vec![Member { peer: key.clone(), state: MemberState::Accepted }]);
                    g.with = Some(key.clone());
                    st.groups.push(g);
                });
                self.join_doc(&gid);
                self.sync_peer_soon(&gid, id);
            }
            (false, true) => {
                self.drop_doc(&gid);
                self.with_state(|st| st.groups.retain(|g| g.id != gid));
            }
            _ => {}
        }
    }

    // ---- groups -----------------------------------------------------------------------

    /// Create an empty group with a random id (and so a random haiku name). Returns the id.
    pub fn create_group(&self) -> String {
        let id = new_group_id();
        self.with_state(|st| st.groups.push(Group::new(id.clone(), Vec::new())));
        self.join_doc(&id);
        id
    }

    /// Local, per-group: download offers automatically, and stop sharing what everybody has.
    pub fn set_group_settings(&self, group: &str, auto_download: Option<bool>, auto_stop: Option<bool>) {
        self.with_state(|st| {
            if let Some(g) = st.groups.iter_mut().find(|g| g.id == group) {
                g.auto_download = auto_download.unwrap_or(g.auto_download);
                g.auto_stop = auto_stop.unwrap_or(g.auto_stop);
            }
        });
        self.auto_jobs(group);
    }

    pub fn rename_group(&self, group: &str, alias: &str) {
        let alias = Some(clean_name(alias)).filter(|a| !a.is_empty());
        self.with_state(|st| {
            if let Some(g) = st.groups.iter_mut().find(|g| g.id == group) {
                g.alias = alias;
            }
        });
    }

    pub fn delete_group(&self, group: &str) {
        // An implied group goes away with the pairing, not on its own.
        if self.0.store.lock().unwrap().state.groups.iter().any(|g| g.id == group && g.with.is_some()) {
            return;
        }
        self.drop_doc(group);
        self.with_state(|st| st.groups.retain(|g| g.id != group));
    }

    /// Put a contact in a group. The invite goes out now if they are connected, otherwise on their
    /// next connection. Adding someone who declined asks them again.
    pub fn add_to_group(&self, group: &str, peer: EndpointId) {
        self.remember_unconnected(peer);
        let key = peer.to_string();
        self.with_state(|st| {
            let Some(g) = st.groups.iter_mut().find(|g| g.id == group) else { return };
            match g.members.iter_mut().find(|m| m.peer == key) {
                Some(m) if m.state == MemberState::Declined => m.state = MemberState::Pending,
                Some(_) => {}
                None => g.members.push(Member { peer: key, state: MemberState::Pending }),
            }
        });
        self.flush_later(peer);
    }

    fn remember_unconnected(&self, id: EndpointId) {
        if !self.0.store.lock().unwrap().state.contacts.iter().any(|c| c.id == id.to_string()) {
            self.remember(id);
        }
    }

    pub fn accept_invite(&self, inviter: EndpointId, group: &str) {
        self.answer_invite(inviter, group, true);
    }

    /// Decline, and remember it: further invites from this peer to this group are not shown.
    pub fn reject_invite(&self, inviter: EndpointId, group: &str) {
        self.answer_invite(inviter, group, false);
    }

    fn answer_invite(&self, inviter: EndpointId, group: &str, accept: bool) {
        let who = inviter.to_string();
        self.with_state(|st| {
            let invite = Invite { inviter: who.clone(), group: group.to_string() };
            st.invites.retain(|i| *i != invite);
            if accept {
                match st.groups.iter_mut().find(|g| g.id == group) {
                    Some(g) => {
                        if !g.members.iter().any(|m| m.peer == who) {
                            g.members.push(Member { peer: who.clone(), state: MemberState::Accepted });
                        }
                    }
                    None => st.groups.push(Group::new(
                        group.to_string(),
                        vec![Member { peer: who.clone(), state: MemberState::Accepted }],
                    )),
                }
            } else if !st.rejected.contains(&invite) {
                st.rejected.push(invite);
            }
            queue_reply(st, &who, group, accept);
        });
        if accept {
            self.join_doc(group);
        }
        self.flush_later(inviter);
    }

    fn on_invite(&self, from: EndpointId, group: String) {
        if parse_gid(&group).is_none() {
            return;
        }
        let who = from.to_string();
        let reply = self.with_state(|st| {
            let invite = Invite { inviter: who.clone(), group: group.clone() };
            if st.rejected.contains(&invite) {
                // Not shown again; just tell them (again) so they stop asking.
                queue_reply(st, &who, &group, false);
                true
            } else if let Some(g) = st.groups.iter_mut().find(|g| g.id == group) {
                // We are already in this group.
                if !g.members.iter().any(|m| m.peer == who) {
                    g.members.push(Member { peer: who.clone(), state: MemberState::Accepted });
                }
                queue_reply(st, &who, &group, true);
                true
            } else {
                if !st.invites.contains(&invite) {
                    st.invites.push(invite);
                }
                false
            }
        });
        if reply {
            self.flush_later(from);
        }
    }

    fn on_reply(&self, from: EndpointId, group: String, accept: bool) {
        let who = from.to_string();
        self.with_state(|st| {
            let state = if accept { MemberState::Accepted } else { MemberState::Declined };
            if let Some(m) = st.groups.iter_mut().find(|g| g.id == group).and_then(|g| g.members.iter_mut().find(|m| m.peer == who)) {
                m.state = state;
            }
        });
        if accept {
            self.sync_peer_soon(&group, from);
        }
    }

    fn flush_later(&self, peer: EndpointId) {
        let this = self.clone();
        tokio::spawn(async move { this.flush(peer).await });
    }

    /// Send everything that was waiting for this peer: undelivered or unanswered invites and
    /// answers to its invites. Receivers de-duplicate, so sending twice is harmless.
    async fn flush(&self, peer: EndpointId) {
        let Some(conn) = self.0.peers.lock().unwrap().get(&peer).and_then(|p| p.conn.clone()) else { return };
        let key = peer.to_string();
        let (invites, replies) = {
            let store = self.0.store.lock().unwrap();
            let invites: Vec<String> = store
                .state
                .groups
                .iter()
                .filter(|g| g.members.iter().any(|m| m.peer == key && matches!(m.state, MemberState::Pending | MemberState::Sent)))
                .map(|g| g.id.clone())
                .collect();
            let replies: Vec<Reply> = store.state.replies.iter().filter(|r| r.peer == key).cloned().collect();
            (invites, replies)
        };
        for group in invites {
            if send_msg(&conn, &Msg::GroupInvite { group: group.clone() }).await.is_err() {
                return;
            }
            self.with_state(|st| {
                if let Some(m) = st.groups.iter_mut().find(|g| g.id == group).and_then(|g| g.members.iter_mut().find(|m| m.peer == key)) {
                    if m.state == MemberState::Pending {
                        m.state = MemberState::Sent;
                    }
                }
            });
        }
        for r in replies {
            if send_msg(&conn, &Msg::GroupReply { group: r.group.clone(), accept: r.accept }).await.is_err() {
                return;
            }
            self.with_state(|st| st.replies.retain(|x| !(x.peer == r.peer && x.group == r.group)));
        }
        // Only now: the peer must have our answer before it will take our documents.
        self.sync_peer(peer).await;
        self.auto_jobs_all();
    }

    // ---- background tasks -------------------------------------------------------------

    fn spawn_background(&self, mut events: impl n0_future::Stream<Item = DiscoveryEvent> + Unpin + Send + 'static) {
        // Accept incoming connections.
        let this = self.clone();
        tokio::spawn(async move {
            while let Some(incoming) = this.0.endpoint.accept().await {
                let this = this.clone();
                tokio::spawn(async move {
                    match incoming.await {
                        Ok(conn) => {
                            this.drop_stale(&conn).await;
                            this.adopt(conn, false);
                        }
                        Err(err) => tracing::debug!("incoming connection failed: {err}"),
                    }
                });
            }
        });

        // LAN announcements.
        let this = self.clone();
        tokio::spawn(async move {
            while let Some(event) = events.next().await {
                match event {
                    DiscoveryEvent::Discovered { endpoint_info, .. } => {
                        let name = endpoint_info
                            .data
                            .user_data()
                            .map(|d| crate::settings::clean_name(d.as_ref()))
                            .filter(|n| !n.is_empty());
                        this.update_peer(endpoint_info.endpoint_id, |p| {
                            p.lan = true;
                            if name.is_some() {
                                p.announced_name = name;
                            }
                        });
                    }
                    DiscoveryEvent::Expired { endpoint_id } => {
                        this.update_peer(endpoint_id, |p| p.lan = false);
                    }
                    _ => {}
                }
            }
        });

        // Peers the BLE transport has identified (full endpoint id known) count as Bluetooth-reachable.
        if let Some(ble) = self.0.ble_transport.clone() {
            let this = self.clone();
            tokio::spawn(async move {
                let mut seen: std::collections::BTreeSet<EndpointId> = Default::default();
                loop {
                    let now: std::collections::BTreeSet<EndpointId> =
                        ble.snapshot_peers().into_iter().filter_map(|p| p.verified_endpoint).collect();
                    for id in now.difference(&seen) {
                        this.update_peer(*id, |p| p.ble_t = true);
                    }
                    for id in seen.difference(&now) {
                        this.update_peer(*id, |p| p.ble_t = false);
                    }
                    seen = now;
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            });
        }

        // Saved contacts always reconnect, backing off while they are unreachable.
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(3)).await;
                let mut ids: Vec<EndpointId> = this
                    .0
                    .store
                    .lock()
                    .unwrap()
                    .state
                    .contacts
                    .iter()
                    .filter_map(|c| c.id.parse().ok())
                    .filter(|id| *id != this.id())
                    .collect();
                // Fellow group members we learned about from a group's document.
                for id in this.doc_peers() {
                    if !ids.contains(&id) && !this.is_forgotten(id) {
                        ids.push(id);
                    }
                }
                this.auto_jobs_all();
                for id in ids {
                    let connected = this.0.peers.lock().unwrap().get(&id).is_some_and(|p| p.conn.is_some());
                    let due = this.0.retry.lock().unwrap().get(&id).is_none_or(|(at, _)| *at <= Instant::now());
                    if connected || !due || !this.0.dialing.lock().unwrap().insert(id) {
                        continue;
                    }
                    let this = this.clone();
                    tokio::spawn(async move {
                        let result = this.connect(id).await;
                        this.0.dialing.lock().unwrap().remove(&id);
                        let mut retry = this.0.retry.lock().unwrap();
                        match result {
                            Ok(()) => {
                                retry.remove(&id);
                            }
                            Err(err) => {
                                tracing::debug!("reconnect to {} failed: {err:#}", id.fmt_short());
                                let delay = retry.get(&id).map_or(Duration::from_secs(5), |(_, d)| (*d * 2).min(Duration::from_secs(60)));
                                retry.insert(id, (Instant::now() + delay, delay));
                            }
                        }
                    });
                }
            }
        });

        // Keep our shareable ticket current.
        let this = self.clone();
        tokio::spawn(async move {
            let mut addrs = this.0.endpoint.watch_addr().stream();
            while let Some(addr) = addrs.next().await {
                let ticket = EndpointTicket::new(addr).to_string();
                *this.0.ticket.lock().unwrap() = ticket.clone();
                this.refresh_ble_payload();
                this.emit(Event::Ticket(ticket));
            }
        });
    }
}

/// Queue an answer for the inviter, replacing any older answer for the same group.
fn queue_reply(st: &mut State, peer: &str, group: &str, accept: bool) {
    st.replies.retain(|r| !(r.peer == peer && r.group == group));
    st.replies.push(Reply { peer: peer.to_string(), group: group.to_string(), accept });
}

/// The id of the implied group of two devices: the same on both, whoever computes it.
fn pair_group_id(a: &EndpointId, b: &EndpointId) -> String {
    let (lo, hi) = if a.as_bytes() <= b.as_bytes() { (a, b) } else { (b, a) };
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"flipflop pair group");
    hasher.update(lo.as_bytes());
    hasher.update(hi.as_bytes());
    hasher.finalize().as_bytes()[..16].iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_gid(hex: &str) -> Option<[u8; 16]> {
    if hex.len() != 32 || !hex.is_ascii() {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// 16 random bytes, hex encoded. (Borrows iroh's secret-key generator as a source of randomness.)
fn new_group_id() -> String {
    iroh::SecretKey::generate().to_bytes()[..16].iter().map(|b| format!("{b:02x}")).collect()
}

fn user_data(name: &str) -> UserData {
    name.parse().unwrap_or_else(|_| UserData::try_from(String::new()).expect("empty fits"))
}

async fn send_msg(conn: &Connection, msg: &Msg) -> Result<()> {
    let bytes = serde_json::to_vec(msg)?;
    let mut tx = conn.open_uni().await.map_err(|e| anyhow!("{e}"))?;
    tx.write_all(&bytes).await.map_err(|e| anyhow!("{e}"))?;
    tx.finish().map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

impl Core {
    pub async fn shutdown(&self) {
        self.save_docs();
        self.0.endpoint.close().await;
        let _ = self.0.dir_lock.unlock();
    }
}
