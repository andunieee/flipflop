//! Shares served from the node's own endpoint.
//!
//! A node has one blob store and one blob provider. Every share is imported
//! into that store and served over the node endpoint's `iroh_blobs::ALPN`,
//! so a transfer reuses whatever path the node already has to the peer (LAN,
//! relay, or any other transport the endpoint gains) instead of minting a
//! throwaway endpoint per send.
//!
//! The provider asks [`ShareRegistry`] before every connection and request:
//! a peer can only fetch a share made for it, and only while that share is
//! open.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{ensure, Context};
use iroh::{EndpointAddr, EndpointId};
use iroh_blobs::api::TempTag;
use iroh_blobs::provider::events::{
    AbortReason, ConnectMode, EventMask, EventSender, ObserveMode, ProviderMessage, RequestMode,
};
use iroh_blobs::store::fs::{options::Options, FsStore};
use iroh_blobs::store::GcConfig;
use iroh_blobs::ticket::BlobTicket;
use iroh_blobs::{BlobFormat, BlobsProtocol, Hash};
use irpc::WithChannels;
use n0_future::task::AbortOnDropHandle;
use crate::protocol::{spawn_share_progress, AppHandle, ShareEvent};
use tokio::sync::mpsc;

use crate::native::import::{canonicalize_input_paths, import_paths};

/// How often the store drops blobs no open share protects any more.
const GC_INTERVAL: Duration = Duration::from_secs(60);

/// The node's outgoing blob store and the provider serving it.
pub(crate) struct ShareRegistry {
    store: FsStore,
    blobs: BlobsProtocol,
    shares: Arc<Mutex<OpenShares>>,
    _dispatcher: AbortOnDropHandle<()>,
}

#[derive(Default)]
struct OpenShares {
    next_id: u64,
    by_id: HashMap<u64, OpenShare>,
}

struct OpenShare {
    hash: Hash,
    peer: EndpointId,
    events: mpsc::Sender<ShareEvent>,
}

impl OpenShares {
    fn serves_peer(&self, peer: &EndpointId) -> bool {
        self.by_id.values().any(|share| share.peer == *peer)
    }

    /// The newest open share of `hash` for `peer`.
    fn find(&self, hash: &Hash, peer: &EndpointId) -> Option<(u64, mpsc::Sender<ShareEvent>)> {
        self.by_id
            .iter()
            .filter(|(_, share)| share.hash == *hash && share.peer == *peer)
            .max_by_key(|(id, _)| **id)
            .map(|(id, share)| (*id, share.events.clone()))
    }
}

impl ShareRegistry {
    /// Opens the store in `dir`, wiping what a previous run left there: those
    /// shares died with it, so nothing in it can be fetched any more.
    pub(crate) async fn open(dir: &Path) -> anyhow::Result<Self> {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e).with_context(|| format!("failed to clear {}", dir.display()))
            }
        }
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
        let mut options = Options::new(dir);
        options.gc = Some(GcConfig {
            interval: GC_INTERVAL,
            add_protected: None,
        });
        let store = FsStore::load_with_opts(dir.join("blobs.db"), options)
            .await
            .with_context(|| format!("failed to load share store at {}", dir.display()))?;

        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let blobs = BlobsProtocol::new(
            &store,
            Some(EventSender::new(
                tx,
                EventMask {
                    connected: ConnectMode::Intercept,
                    get: RequestMode::InterceptLog,
                    get_many: RequestMode::Disabled,
                    push: RequestMode::Disabled,
                    observe: ObserveMode::None,
                    ..EventMask::DEFAULT
                },
            )),
        );
        let shares = Arc::new(Mutex::new(OpenShares::default()));
        let dispatcher =
            AbortOnDropHandle::new(n0_future::task::spawn(dispatch(rx, shares.clone())));

        Ok(Self {
            store,
            blobs,
            shares,
            _dispatcher: dispatcher,
        })
    }

    /// The provider to mount on the node's router under `iroh_blobs::ALPN`.
    pub(crate) fn protocol(&self) -> BlobsProtocol {
        self.blobs.clone()
    }

    /// Imports `paths` and opens a share of them for `peer`. `addr` is the
    /// node's own address, which the ticket carries.
    pub(crate) async fn share(
        &self,
        addr: EndpointAddr,
        peer: EndpointId,
        paths: Vec<PathBuf>,
        app_handle: AppHandle,
    ) -> anyhow::Result<NodeShare> {
        ensure!(!paths.is_empty(), "no paths provided for sharing");
        let paths = canonicalize_input_paths(paths)?;
        let entry_type = if paths.len() > 1 {
            "collection"
        } else if paths[0].is_dir() {
            "directory"
        } else {
            "file"
        };
        let (temp_tag, size, _collection) = import_paths(paths, &self.store).await?;
        let hash = temp_tag.hash();

        let (events_tx, events_rx) = mpsc::channel(64);
        let completed_peers = Arc::new(AtomicUsize::new(0));
        let progress = spawn_share_progress(events_rx, app_handle, size, completed_peers.clone());
        let id = {
            let mut shares = self.shares.lock().expect("shares");
            let id = shares.next_id;
            shares.next_id += 1;
            shares.by_id.insert(
                id,
                OpenShare {
                    hash,
                    peer,
                    events: events_tx,
                },
            );
            id
        };

        Ok(NodeShare {
            ticket: BlobTicket::new(addr, hash, BlobFormat::HashSeq).to_string(),
            hash: hash.to_hex().to_string(),
            size,
            entry_type: entry_type.to_string(),
            completed_peers,
            _registration: Registration {
                shares: self.shares.clone(),
                id,
            },
            _progress: progress,
            _temp_tag: temp_tag,
        })
    }
}

/// An open share. Dropping it stops serving: the peer's further requests are
/// refused and any transfer still running is aborted.
pub struct NodeShare {
    pub ticket: String,
    pub hash: String,
    pub size: u64,
    pub entry_type: String,
    completed_peers: Arc<AtomicUsize>,
    // Dropped in this order: refuse new requests, abort the running ones
    // (their update streams live in the progress task), then let the store
    // collect the blobs.
    _registration: Registration,
    _progress: AbortOnDropHandle<anyhow::Result<()>>,
    _temp_tag: TempTag,
}

impl NodeShare {
    /// Peers that pulled the entire payload so far.
    pub fn completed_peers(&self) -> usize {
        self.completed_peers.load(Ordering::SeqCst)
    }
}

impl std::fmt::Debug for NodeShare {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeShare")
            .field("hash", &self.hash)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

struct Registration {
    shares: Arc<Mutex<OpenShares>>,
    id: u64,
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Ok(mut shares) = self.shares.lock() {
            shares.by_id.remove(&self.id);
        }
    }
}

/// Answers the provider's connection and request checks, and hands each
/// allowed request to the progress task of the share it is for.
async fn dispatch(mut rx: mpsc::Receiver<ProviderMessage>, shares: Arc<Mutex<OpenShares>>) {
    // Who is on each provider connection, and which shares were told so.
    let mut peers: HashMap<u64, EndpointId> = HashMap::new();
    let mut announced: HashSet<(u64, u64)> = HashSet::new();

    while let Some(message) = rx.recv().await {
        match message {
            ProviderMessage::ClientConnected(msg) => {
                let peer = msg
                    .endpoint_id
                    .filter(|id| shares.lock().expect("shares").serves_peer(id));
                let verdict = match peer {
                    Some(peer) => {
                        peers.insert(msg.connection_id, peer);
                        Ok(())
                    }
                    None => {
                        tracing::debug!(
                            peer = ?msg.endpoint_id,
                            "share: refusing a connection from a peer with no open share"
                        );
                        Err(AbortReason::Permission)
                    }
                };
                msg.tx.send(verdict).await.ok();
            }
            ProviderMessage::ConnectionClosed(msg) => {
                peers.remove(&msg.connection_id);
                announced.retain(|(connection_id, _)| *connection_id != msg.connection_id);
            }
            ProviderMessage::GetRequestReceived(msg) => {
                let WithChannels {
                    inner, tx, rx: updates, ..
                } = msg;
                let peer = peers.get(&inner.connection_id).copied();
                let target = peer.and_then(|peer| {
                    shares
                        .lock()
                        .expect("shares")
                        .find(&inner.request.hash, &peer)
                        .map(|(id, events)| (peer, id, events))
                });
                let verdict = match target {
                    Some((peer, id, events)) => {
                        let mut delivered = true;
                        if announced.insert((inner.connection_id, id)) {
                            delivered = events.send(ShareEvent::PeerConnected(peer)).await.is_ok();
                        }
                        let request = ShareEvent::Request {
                            connection_id: inner.connection_id,
                            request_id: inner.request_id,
                            ranges: inner.request.ranges.clone(),
                            updates,
                        };
                        // Fails only if the share closed since the lookup.
                        if delivered && events.send(request).await.is_ok() {
                            Ok(())
                        } else {
                            Err(AbortReason::Permission)
                        }
                    }
                    None => {
                        tracing::debug!(
                            ?peer,
                            hash = %inner.request.hash.fmt_short(),
                            "share: refusing a request for nothing shared with this peer"
                        );
                        Err(AbortReason::Permission)
                    }
                };
                tx.send(verdict).await.ok();
            }
            // Every other request kind is disabled in the event mask.
            _ => {}
        }
    }
}
