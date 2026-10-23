//! Networking core: one iroh endpoint, LAN discovery (mDNS), peer bookkeeping and a tiny
//! hello/chat protocol. UI-agnostic; the UI subscribes through the event callback.

use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
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

use crate::settings::Settings;

pub const ALPN: &[u8] = b"flipflop/0";
const MDNS_SERVICE: &str = "flipflop";
const MAX_MSG_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct Peer {
    pub id: EndpointId,
    pub name: String,
    pub lan: bool,
    pub ble: bool,
    pub connected: bool,
}

#[derive(Debug, Clone)]
pub enum Event {
    Peers(Vec<Peer>),
    /// Our own shareable ticket changed (new addresses, relay came up…).
    Ticket(String),
    /// Our own name changed (also fired once at start-up).
    Name(String),
    Message { peer: EndpointId, mine: bool, text: String },
    Notice(String),
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Msg {
    Hello { name: String },
    Text { text: String },
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
    endpoint: Endpoint,
    ble_transport: Option<Arc<BleTransport>>,
    ble_transport_status: String,
    memory: MemoryLookup,
    settings: Mutex<Settings>,
    peers: Mutex<BTreeMap<EndpointId, PeerEntry>>,
    ticket: Mutex<String>,
    ble_payload: watch::Sender<Option<BlePayload>>,
    on_event: Box<dyn Fn(Event) + Send + Sync>,
}

#[derive(Clone)]
pub struct Core(Arc<Inner>);

impl Core {
    /// Must be called from within a tokio runtime. With `ble_transport`, iroh also gets Bluetooth LE
    /// as a network path (custom transports can only be added at bind time, so on Android the
    /// Bluetooth permission must already be granted).
    pub async fn start(
        data_dir: &Path,
        ble_transport: bool,
        on_event: impl Fn(Event) + Send + Sync + 'static,
    ) -> Result<Core> {
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
            endpoint,
            ble_transport: ble,
            ble_transport_status,
            memory,
            settings: Mutex::new(settings),
            peers: Mutex::default(),
            ticket: Mutex::default(),
            ble_payload: watch::channel(None).0,
            on_event: Box::new(on_event),
        }));
        core.emit(Event::Name(name));
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

    fn publish_peers(&self) {
        let peers = self.0.peers.lock().unwrap();
        let mut list: Vec<Peer> = peers
            .iter()
            .map(|(id, p)| Peer {
                id: *id,
                name: p
                    .hello_name
                    .clone()
                    .or_else(|| p.announced_name.clone())
                    .unwrap_or_else(|| id.fmt_short().to_string()),
                lan: p.lan,
                ble: p.ble || p.ble_t,
                connected: p.conn.is_some(),
            })
            .collect();
        drop(peers);
        list.sort_by(|a, b| b.connected.cmp(&a.connected).then_with(|| a.name.cmp(&b.name)));
        self.emit(Event::Peers(list));
    }

    /// Update a peer entry and republish the list. Dead entries (not connected, no source) are removed.
    fn update_peer(&self, id: EndpointId, f: impl FnOnce(&mut PeerEntry)) {
        if id == self.id() {
            return;
        }
        {
            let mut peers = self.0.peers.lock().unwrap();
            let entry = peers.entry(id).or_default();
            f(entry);
            if !entry.lan && !entry.ble && !entry.ble_t && entry.conn.is_none() {
                peers.remove(&id);
            }
        }
        self.publish_peers();
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
        Ok(id)
    }

    pub async fn connect(&self, id: EndpointId) -> Result<()> {
        if self.0.peers.lock().unwrap().get(&id).is_some_and(|p| p.conn.is_some()) {
            return Ok(());
        }
        let conn = self
            .0
            .endpoint
            .connect(id, ALPN)
            .await
            .map_err(|e| anyhow!("could not connect: {e}"))?;
        self.adopt(conn);
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
    fn adopt(&self, conn: Connection) {
        let id = conn.remote_id();
        let stable = conn.stable_id();
        let previous = {
            let mut peers = self.0.peers.lock().unwrap();
            peers.entry(id).or_default().conn.replace(conn.clone())
        };
        if let Some(old) = previous {
            old.close(0u32.into(), b"replaced");
        }
        self.publish_peers();
        self.emit(Event::Notice(format!("connected to {}", id.fmt_short())));

        let this = self.clone();
        tokio::spawn(async move {
            let hello = Msg::Hello { name: this.name() };
            if let Err(err) = send_msg(&conn, &hello).await {
                tracing::debug!("hello failed: {err:#}");
            }
            while let Ok(mut rx) = conn.accept_uni().await {
                let Ok(bytes) = rx.read_to_end(MAX_MSG_BYTES).await else { continue };
                match serde_json::from_slice::<Msg>(&bytes) {
                    Ok(Msg::Hello { name }) => {
                        let name = crate::settings::clean_name(&name);
                        this.update_peer(id, |p| p.hello_name = Some(name));
                    }
                    Ok(Msg::Text { text }) => {
                        this.emit(Event::Message { peer: id, mine: false, text });
                    }
                    Err(err) => tracing::debug!("bad message: {err}"),
                }
            }
            // Only clear the slot if it still holds *this* connection (not a replacement).
            this.update_peer(id, |p| {
                if p.conn.as_ref().is_some_and(|c| c.stable_id() == stable) {
                    p.conn = None;
                    p.hello_name = None;
                }
            });
            this.emit(Event::Notice(format!("disconnected from {}", id.fmt_short())));
        });
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
                        Ok(conn) => this.adopt(conn),
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
        self.0.endpoint.close().await;
    }
}
