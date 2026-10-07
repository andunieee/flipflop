//! Bluetooth as an iroh transport.
//!
//! iroh carries QUIC packets over "custom transports"; this module is one
//! whose packets travel over Bluetooth links. The node endpoint adds it next
//! to IP and relay, so every protocol (control, transfers) can use it, and a
//! [`PreferIpThenRelay`] path selector keeps it as the path of last resort:
//! Bluetooth is slow, and only carries traffic when neither the LAN nor the
//! relay can reach the peer.
//!
//! The platform half is a [`BluetoothLink`]: it advertises this device,
//! finds nearby ones, keeps the links, and moves opaque packets. Peers are
//! named by a [`PeerTag`], a prefix of their endpoint id. Android hides the
//! device's own Bluetooth address, so the tag (advertised by the link) is the
//! only stable name a peer has; QUIC authenticates the full endpoint id once
//! packets flow, so a colliding or spoofed tag can only fail a handshake.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use iroh::address_lookup::{AddressLookup, EndpointData, EndpointInfo, Error as LookupError, Item};
use iroh::endpoint::transports::{
    Addr, CustomEndpoint, CustomSender, CustomTransport, PathSelection, PathSelectionContext,
    PathSelector, RecvInfo, Transmit,
};
use iroh::{EndpointId, TransportAddr};
use iroh_base::CustomAddr;
use n0_future::stream::Boxed as BoxStream;
use n0_future::StreamExt;
use tokio::sync::{mpsc, watch};

/// Custom transport id of [`BluetoothHub`] addresses ("TMBT").
pub const TRANSPORT_ID: u64 = 0x544d_4254;

/// Length of a [`PeerTag`]. Short enough that the tag plus the link's channel
/// number fit a legacy BLE advertisement next to a 128-bit service UUID.
pub const PEER_TAG_LEN: usize = 6;

/// A peer's name on Bluetooth: the first [`PEER_TAG_LEN`] bytes of its
/// endpoint id.
pub type PeerTag = [u8; PEER_TAG_LEN];

/// How long a sighting keeps a peer dialable after the link last saw it.
const SEEN_FOR: Duration = Duration::from_secs(60);

/// Received packets waiting for the endpoint. Excess packets are dropped,
/// as a full UDP socket would; QUIC retransmits.
const RECV_QUEUE: usize = 256;

pub fn peer_tag(id: &EndpointId) -> PeerTag {
    let mut tag = [0u8; PEER_TAG_LEN];
    tag.copy_from_slice(&id.as_bytes()[..PEER_TAG_LEN]);
    tag
}

fn custom_addr(tag: &PeerTag) -> CustomAddr {
    CustomAddr::from_parts(TRANSPORT_ID, tag)
}

fn tag_of(addr: &CustomAddr) -> Option<PeerTag> {
    if addr.id() != TRANSPORT_ID {
        return None;
    }
    addr.data().try_into().ok()
}

/// The platform half of the transport.
///
/// Implementations must not block in [`BluetoothLink::send`]: it runs on
/// iroh's send path. Packets may be dropped (they are datagrams), but must
/// arrive whole and in one piece each.
pub trait BluetoothLink: fmt::Debug + Send + Sync + 'static {
    /// Starts advertising `local`, discovering peers and accepting links.
    /// Called once. Sightings, packets and adapter state go to `hub`.
    fn start(&self, local: PeerTag, hub: BluetoothHub);

    /// Queues `packet` for the peer named `peer`, linking to it first if
    /// needed (it was reported through [`BluetoothHub::peer_seen`]).
    fn send(&self, peer: PeerTag, packet: &[u8]);
}

/// The shared core between a [`BluetoothLink`] and the node endpoint.
/// Cheap to clone; survives endpoint rebuilds.
#[derive(Clone)]
pub struct BluetoothHub {
    inner: Arc<Inner>,
}

struct Inner {
    link: Arc<dyn BluetoothLink>,
    local: OnceLock<PeerTag>,
    available: Mutex<bool>,
    local_addrs: n0_watcher::Watchable<Vec<CustomAddr>>,
    packets_tx: mpsc::Sender<(PeerTag, Vec<u8>)>,
    packets_rx: Mutex<mpsc::Receiver<(PeerTag, Vec<u8>)>>,
    seen: watch::Sender<HashMap<PeerTag, Instant>>,
}

impl fmt::Debug for BluetoothHub {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BluetoothHub")
            .field("link", &self.inner.link)
            .finish_non_exhaustive()
    }
}

impl BluetoothHub {
    pub fn new(link: Arc<dyn BluetoothLink>) -> Self {
        let (packets_tx, packets_rx) = mpsc::channel(RECV_QUEUE);
        Self {
            inner: Arc::new(Inner {
                link,
                local: OnceLock::new(),
                available: Mutex::new(false),
                local_addrs: n0_watcher::Watchable::new(Vec::new()),
                packets_tx,
                packets_rx: Mutex::new(packets_rx),
                seen: watch::Sender::new(HashMap::new()),
            }),
        }
    }

    // ------------------------------------------------- from the platform

    /// Whether the adapter is on and the link is accepting. While it isn't,
    /// the endpoint advertises no Bluetooth address.
    pub fn set_available(&self, available: bool) {
        *self.inner.available.lock().expect("available") = available;
        self.publish_local_addr();
        if !available {
            self.inner.seen.send_modify(HashMap::clear);
        }
    }

    /// A packet arrived from `from`.
    pub fn deliver(&self, from: PeerTag, packet: Vec<u8>) {
        // Full queue: drop, like a UDP socket would.
        let _ = self.inner.packets_tx.try_send((from, packet));
    }

    /// The link saw `peer` nearby and can link to it.
    pub fn peer_seen(&self, peer: PeerTag) {
        self.inner.seen.send_modify(|seen| {
            seen.insert(peer, Instant::now());
        });
    }

    // ----------------------------------------------------- for the node

    /// Starts the link for this node (once; later calls are no-ops).
    pub(crate) fn attach(&self, local: EndpointId) {
        let tag = peer_tag(&local);
        if self.inner.local.set(tag).is_ok() {
            self.publish_local_addr();
            self.inner.link.start(tag, self.clone());
        }
    }

    pub(crate) fn transport(&self) -> Arc<dyn CustomTransport> {
        Arc::new(self.clone())
    }

    pub(crate) fn address_lookup(&self) -> BluetoothLookup {
        BluetoothLookup { hub: self.clone() }
    }

    fn publish_local_addr(&self) {
        let available = *self.inner.available.lock().expect("available");
        let addrs = match self.inner.local.get() {
            Some(tag) if available => vec![custom_addr(tag)],
            _ => Vec::new(),
        };
        let _ = self.inner.local_addrs.set(addrs);
    }

    fn seen_recently(seen: &HashMap<PeerTag, Instant>, tag: &PeerTag) -> bool {
        seen.get(tag).is_some_and(|at| at.elapsed() < SEEN_FOR)
    }
}

impl CustomTransport for BluetoothHub {
    fn bind(&self) -> io::Result<Box<dyn CustomEndpoint>> {
        Ok(Box::new(BluetoothEndpoint { hub: self.clone() }))
    }
}

/// One endpoint's view of the hub. A rebuilt node endpoint binds a new one
/// over the same hub, picking up where the old one left off.
#[derive(Debug)]
struct BluetoothEndpoint {
    hub: BluetoothHub,
}

impl CustomEndpoint for BluetoothEndpoint {
    fn watch_local_addrs(&self) -> n0_watcher::Direct<Vec<CustomAddr>> {
        self.hub.inner.local_addrs.watch()
    }

    fn create_sender(&self) -> Arc<dyn CustomSender> {
        Arc::new(BluetoothSender {
            hub: self.hub.clone(),
        })
    }

    fn poll_recv(
        &mut self,
        cx: &mut Context,
        bufs: &mut [io::IoSliceMut<'_>],
        metas: &mut [noq_udp::RecvMeta],
        recv_infos: &mut [RecvInfo],
    ) -> Poll<io::Result<usize>> {
        let n = bufs.len().min(metas.len()).min(recv_infos.len());
        if n == 0 {
            return Poll::Ready(Ok(0));
        }
        let local = self.hub.inner.local.get().map(custom_addr);
        let mut rx = self.hub.inner.packets_rx.lock().expect("packets_rx");
        let mut packets = Vec::with_capacity(n);
        match rx.poll_recv_many(cx, &mut packets, n) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(0) => return Poll::Ready(Err(io::Error::other("bluetooth hub closed"))),
            Poll::Ready(_) => {}
        }
        let mut filled = 0;
        for (from, packet) in packets {
            let buf = &mut bufs[filled];
            if packet.len() > buf.len() {
                tracing::debug!(len = packet.len(), "bluetooth: dropping an oversized packet");
                continue;
            }
            buf[..packet.len()].copy_from_slice(&packet);
            metas[filled].len = packet.len();
            metas[filled].stride = packet.len();
            recv_infos[filled] = RecvInfo::new(custom_addr(&from), local.clone());
            filled += 1;
        }
        if filled == 0 {
            // Only oversized packets: nothing queued woke us, so ask again.
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        Poll::Ready(Ok(filled))
    }

    fn max_transmit_segments(&self) -> NonZeroUsize {
        NonZeroUsize::MIN
    }
}

#[derive(Debug)]
struct BluetoothSender {
    hub: BluetoothHub,
}

impl CustomSender for BluetoothSender {
    fn is_valid_send_addr(&self, addr: &CustomAddr) -> bool {
        tag_of(addr).is_some()
    }

    fn poll_send(
        &self,
        _cx: &mut Context,
        dst: &CustomAddr,
        _src: Option<&CustomAddr>,
        transmit: &Transmit<'_>,
    ) -> Poll<io::Result<()>> {
        let Some(tag) = tag_of(dst) else {
            return Poll::Ready(Err(io::Error::other("not a bluetooth address")));
        };
        let segment = transmit.segment_size.unwrap_or(transmit.contents.len()).max(1);
        for packet in transmit.contents.chunks(segment) {
            self.hub.inner.link.send(tag, packet);
        }
        Poll::Ready(Ok(()))
    }
}

/// Resolves an endpoint id to its Bluetooth address once the link has seen
/// that peer nearby. The stream waits for a sighting rather than ending, so
/// a peer that comes into range while a dial is pending is still found.
#[derive(Debug)]
pub(crate) struct BluetoothLookup {
    hub: BluetoothHub,
}

impl AddressLookup for BluetoothLookup {
    fn resolve(&self, endpoint_id: EndpointId) -> Option<BoxStream<Result<Item, LookupError>>> {
        let tag = peer_tag(&endpoint_id);
        if self.hub.inner.local.get() == Some(&tag) {
            return None;
        }
        let mut seen = self.hub.inner.seen.subscribe();
        let sighting = async move {
            loop {
                if BluetoothHub::seen_recently(&seen.borrow_and_update(), &tag) {
                    return Some(Ok(Item::new(
                        EndpointInfo {
                            endpoint_id,
                            data: EndpointData::from_iter([TransportAddr::Custom(custom_addr(
                                &tag,
                            ))]),
                        },
                        "bluetooth",
                        None,
                    )));
                }
                if seen.changed().await.is_err() {
                    return None;
                }
            }
        };
        Some(Box::pin(
            n0_future::stream::once_future(sighting).filter_map(|item| item),
        ))
    }
}

/// Picks direct IP paths first, then the relay, then Bluetooth; the lowest
/// RTT within a tier. Within a tier it only switches for a clear RTT win,
/// so jitter doesn't flap the path (as iroh's default selector does).
#[derive(Debug, Default)]
pub(crate) struct PreferIpThenRelay;

/// RTT a same-tier candidate must beat the current path by to take over.
const SWITCH_MIN_RTT_GAIN: Duration = Duration::from_millis(5);

fn tier(addr: &Addr) -> u8 {
    match addr {
        Addr::Ip(_) => 0,
        Addr::Relay(..) => 1,
        Addr::Custom(_) => 2,
    }
}

impl PathSelector for PreferIpThenRelay {
    fn select(&self, ctx: &PathSelectionContext<'_>) -> PathSelection {
        let current = ctx.current();
        let mut best = None;
        let mut current_key: Option<(u8, Duration)> = None;
        for path in ctx.paths() {
            let Some(stats) = path.stats() else {
                continue;
            };
            let key = (tier(&path.network_path().remote()), stats.rtt);
            if Some(path.network_path()) == current && current_key.is_none_or(|c| key < c) {
                current_key = Some(key);
            }
            if best.as_ref().is_none_or(|(_, b)| key < *b) {
                best = Some((path, key));
            }
        }

        let mut selection = PathSelection::none();
        let Some((best_path, (best_tier, best_rtt))) = best else {
            return selection;
        };
        match current_key {
            Some((current_tier, current_rtt))
                if current_tier == best_tier && best_rtt + SWITCH_MIN_RTT_GAIN > current_rtt => {}
            _ => selection.set(&best_path),
        }
        selection
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_endpoint_id_prefixes() {
        let id = iroh::SecretKey::generate().public();
        let tag = peer_tag(&id);
        assert_eq!(&tag[..], &id.as_bytes()[..PEER_TAG_LEN]);
        assert_eq!(tag_of(&custom_addr(&tag)), Some(tag));
    }

    #[test]
    fn other_transports_addresses_are_not_bluetooth() {
        assert_eq!(tag_of(&CustomAddr::from_parts(0x20, &[0; PEER_TAG_LEN])), None);
        assert_eq!(tag_of(&CustomAddr::from_parts(TRANSPORT_ID, &[0; 3])), None);
    }

    /// The air between fake links: every started hub sees every other one.
    #[derive(Debug, Default)]
    struct FakeAir {
        hubs: Mutex<HashMap<PeerTag, BluetoothHub>>,
    }

    /// A [`BluetoothLink`] over [`FakeAir`], keeping the platform contract:
    /// whole packets, tagged with the sender.
    #[derive(Debug)]
    struct FakeLink {
        air: Arc<FakeAir>,
        local: OnceLock<PeerTag>,
        /// Start without being seen, to come into range later.
        hidden: bool,
    }

    impl FakeLink {
        fn new(air: &Arc<FakeAir>, hidden: bool) -> Arc<Self> {
            Arc::new(Self {
                air: air.clone(),
                local: OnceLock::new(),
                hidden,
            })
        }
    }

    impl BluetoothLink for FakeLink {
        fn start(&self, local: PeerTag, hub: BluetoothHub) {
            self.local.set(local).unwrap();
            hub.set_available(true);
            let mut hubs = self.air.hubs.lock().unwrap();
            for (tag, other) in hubs.iter() {
                hub.peer_seen(*tag);
                if !self.hidden {
                    other.peer_seen(local);
                }
            }
            hubs.insert(local, hub);
        }

        fn send(&self, peer: PeerTag, packet: &[u8]) {
            let hubs = self.air.hubs.lock().unwrap();
            if let Some(hub) = hubs.get(&peer) {
                hub.deliver(*self.local.get().unwrap(), packet.to_vec());
            }
        }
    }

    const ECHO: &[u8] = b"test/bt-echo";

    #[derive(Debug, Clone)]
    struct Echo;

    impl iroh::protocol::ProtocolHandler for Echo {
        async fn accept(
            &self,
            connection: iroh::endpoint::Connection,
        ) -> Result<(), iroh::protocol::AcceptError> {
            let (mut send, mut recv) = connection.accept_bi().await?;
            tokio::io::copy(&mut recv, &mut send).await?;
            send.finish()?;
            connection.closed().await;
            Ok(())
        }
    }

    /// An endpoint whose only transport is Bluetooth over `link`.
    async fn bluetooth_only_endpoint(link: Arc<FakeLink>) -> (iroh::Endpoint, BluetoothHub) {
        let key = iroh::SecretKey::generate();
        let hub = BluetoothHub::new(link);
        hub.attach(key.public());
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(key)
            .relay_mode(iroh::RelayMode::Disabled)
            .clear_ip_transports()
            .add_custom_transport(hub.transport())
            .address_lookup(hub.address_lookup())
            .path_selector(Arc::new(PreferIpThenRelay))
            .alpns(vec![ECHO.to_vec()])
            .bind()
            .await
            .expect("bind");
        (endpoint, hub)
    }

    async fn echo_over(client: &iroh::Endpoint, server: EndpointId, payload: &[u8]) -> Vec<u8> {
        // Dial by id alone: the address comes from the Bluetooth lookup.
        let conn = client.connect(server, ECHO).await.expect("connect");
        let (mut send, mut recv) = conn.open_bi().await.expect("open_bi");
        send.write_all(payload).await.expect("write");
        send.finish().expect("finish");
        let echoed = recv.read_to_end(payload.len() + 1).await.expect("read");
        assert!(
            conn.paths().iter().any(|p| p.is_selected() && p.remote_addr().is_custom()),
            "the connection runs over bluetooth"
        );
        conn.close(0u32.into(), b"done");
        echoed
    }

    #[tokio::test]
    async fn peers_reach_each_other_over_bluetooth_alone() {
        let air = Arc::new(FakeAir::default());
        let (client, _client_hub) = bluetooth_only_endpoint(FakeLink::new(&air, false)).await;
        let (server, _server_hub) = bluetooth_only_endpoint(FakeLink::new(&air, false)).await;
        let server_id = server.id();
        let router = iroh::protocol::Router::builder(server).accept(ECHO, Echo).spawn();

        let payload: Vec<u8> = (0..1_000_000u32).map(|i| i as u8).collect();
        let echoed = tokio::time::timeout(
            Duration::from_secs(30),
            echo_over(&client, server_id, &payload),
        )
        .await
        .expect("echo over bluetooth timed out");
        assert_eq!(echoed, payload);

        router.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn a_dial_waits_for_the_peer_to_come_into_range() {
        let air = Arc::new(FakeAir::default());
        let (client, client_hub) = bluetooth_only_endpoint(FakeLink::new(&air, false)).await;
        // Started after the client, and unseen by it so far.
        let (server, _server_hub) = bluetooth_only_endpoint(FakeLink::new(&air, true)).await;
        let server_id = server.id();
        let router = iroh::protocol::Router::builder(server).accept(ECHO, Echo).spawn();

        let dial = tokio::spawn({
            let client = client.clone();
            async move { echo_over(&client, server_id, b"late").await }
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!dial.is_finished(), "nothing to dial before the peer is seen");
        client_hub.peer_seen(peer_tag(&server_id));

        let echoed = tokio::time::timeout(Duration::from_secs(30), dial)
            .await
            .expect("dial after sighting timed out")
            .expect("dial task");
        assert_eq!(echoed, b"late");

        router.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn a_direct_ip_path_wins_over_bluetooth() {
        let air = Arc::new(FakeAir::default());
        let endpoint = |link: Arc<FakeLink>| async move {
            let key = iroh::SecretKey::generate();
            let hub = BluetoothHub::new(link);
            hub.attach(key.public());
            let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                .secret_key(key)
                .relay_mode(iroh::RelayMode::Disabled)
                .add_custom_transport(hub.transport())
                .address_lookup(hub.address_lookup())
                .path_selector(Arc::new(PreferIpThenRelay))
                .alpns(vec![ECHO.to_vec()])
                .bind()
                .await
                .expect("bind");
            (endpoint, hub)
        };
        let (client, _client_hub) = endpoint(FakeLink::new(&air, false)).await;
        let (server, _server_hub) = endpoint(FakeLink::new(&air, false)).await;
        let server_addr = server.addr();
        let router = iroh::protocol::Router::builder(server).accept(ECHO, Echo).spawn();

        let conn = client.connect(server_addr, ECHO).await.expect("connect");
        let ip_selected = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let paths = conn.paths();
                let both = paths.iter().any(|p| p.remote_addr().is_custom())
                    && paths.iter().any(|p| p.remote_addr().is_ip());
                if both && paths.iter().any(|p| p.is_selected() && p.remote_addr().is_ip()) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        assert!(
            ip_selected.is_ok(),
            "with both paths open, IP must carry the traffic: {:?}",
            conn.paths()
                .iter()
                .map(|p| (p.remote_addr().clone(), p.is_selected()))
                .collect::<Vec<_>>()
        );
        conn.close(0u32.into(), b"done");
        router.shutdown().await.expect("shutdown");
    }

    #[test]
    fn an_unavailable_adapter_reports_no_local_address() {
        let air = Arc::new(FakeAir::default());
        let hub = BluetoothHub::new(FakeLink::new(&air, false));
        let id = iroh::SecretKey::generate().public();
        hub.attach(id);
        let local = || hub.inner.local_addrs.get();
        assert_eq!(local(), vec![custom_addr(&peer_tag(&id))]);

        hub.set_available(false);
        assert!(local().is_empty());
        hub.set_available(true);
        assert_eq!(local(), vec![custom_addr(&peer_tag(&id))]);
    }

    #[test]
    fn bluetooth_ranks_below_ip_and_relay() {
        let ip = Addr::Ip("127.0.0.1:1".parse().unwrap());
        let bt = Addr::Custom(custom_addr(&[1; PEER_TAG_LEN]));
        assert!(tier(&ip) < tier(&bt));
    }
}
