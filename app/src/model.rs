//! Everything the UI shows that comes from the node, as the core reports it, in a radio station so
//! each part of the UI only redraws for what it shows.

use std::collections::HashMap;

use flipflop::{
    core::{
        Core, GroupInfo, GroupView, GroupsView, MemberState, Peer, TransferItem, Unseen,
    },
    format,
};
use freya::radio::*;

#[derive(Default)]
pub struct Model {
    pub core: Option<Core>,
    pub name: String,
    pub ticket: String,
    pub ble_status: String,
    pub download_dir: String,
    /// The webcam is looking for a code (desktop).
    pub scanning: bool,
    /// Room taken by the system bars and the keyboard: left, top, right, bottom (Android).
    pub insets: [f32; 4],

    pub peers: Vec<Peer>,
    pub groups: GroupsView,
    pub views: HashMap<String, GroupView>,
    /// What is new in each group (Home).
    pub home: Vec<Unseen>,
    /// What is moving, in every group (Home).
    pub transfers: Vec<TransferItem>,
}

#[derive(PartialEq, Eq, Clone, Copy, Debug, Hash)]
pub enum Channel {
    /// This device: name, ticket, status, insets.
    Me,
    Peers,
    /// The groups, invites and the groups' contents.
    Groups,
    /// News and transfers.
    Home,
}

impl RadioChannel<Model> for Channel {}

/// A peer as shown: its alias if set, else its own name.
pub fn peer_title(p: &Peer) -> String {
    p.alias.clone().unwrap_or_else(|| p.name.clone())
}

/// Own name (dimmed when an alias hides it) and key prefix.
pub fn peer_subtitle(p: &Peer) -> String {
    let short = p.id.fmt_short().to_string();
    match &p.alias {
        Some(_) => format!("{} · {short}", p.name),
        None => short,
    }
}

/// They paired with us and we have not answered: shown on Home.
pub fn pair_request(p: &Peer) -> bool {
    p.paired_us && !p.paired && !p.pair_ignored
}

/// An implied group is named after the other device.
pub fn group_title(g: &GroupInfo) -> String {
    match g.with.and_then(|w| g.members.iter().find(|m| m.id == w)) {
        Some(m) => m.name.clone(),
        None => g.alias.clone().unwrap_or_else(|| g.haiku.clone()),
    }
}

impl Model {
    pub fn group(&self, id: &str) -> Option<&GroupInfo> {
        self.groups.groups.iter().find(|g| g.id == id)
    }

    /// The listed groups (implied ones are the peers' pages).
    pub fn listed_groups(&self) -> impl Iterator<Item = &GroupInfo> {
        self.groups.groups.iter().filter(|g| g.with.is_none())
    }

    /// Paired peers, which have a page.
    pub fn paired_peers(&self) -> impl Iterator<Item = &Peer> {
        self.peers.iter().filter(|p| p.pair_group.is_some())
    }

    /// Some other member of the group is online.
    pub fn group_online(&self, id: &str) -> bool {
        self.views
            .get(id)
            .is_some_and(|v| v.members.iter().any(|m| m.online && !m.me))
    }

    /// "x/y members online".
    pub fn group_summary(&self, id: &str) -> String {
        let view = self.views.get(id);
        let joined = view.map_or(1, |v| v.members.len().max(1));
        let online = view.map_or(1, |v| {
            v.members.iter().filter(|m| m.online || m.me).count().max(1)
        });
        format!("{online}/{joined} members online")
    }

    /// The count on Home: news, invites and pair requests.
    pub fn home_badge(&self) -> usize {
        self.home.len() + self.groups.invites.len() + self.peers.iter().filter(|p| pair_request(p)).count()
    }

    /// Home shows these news (Unseen may be empty after a mark-seen raced an update).
    pub fn news(&self) -> impl Iterator<Item = &Unseen> {
        self.home.iter().filter(|n| !n.is_empty())
    }

    /// The groups a peer's page offers to invite the peer to, in the order of the drop-down: the listed
    /// groups the peer is not part of (a declined invite can be sent again).
    pub fn invitable(&self, peer: iroh::EndpointId) -> Vec<&GroupInfo> {
        let key = peer.to_string();
        self.listed_groups()
            .filter(|g| {
                !g.members
                    .iter()
                    .any(|m| m.id == peer && m.state != MemberState::Declined)
            })
            .filter(|g| {
                !self
                    .views
                    .get(&g.id)
                    .is_some_and(|v| v.members.iter().any(|m| m.id == key))
            })
            .collect()
    }

    /// Members of a group: who the document lists, then invitees that have not joined.
    pub fn members(&self, g: &GroupInfo) -> Vec<MemberRow> {
        let mut rows: Vec<MemberRow> = self
            .views
            .get(&g.id)
            .map(|v| {
                v.members
                    .iter()
                    .map(|m| MemberRow {
                        id: m.id.clone(),
                        name: m.name.clone(),
                        status: if m.me {
                            "this device"
                        } else if m.online {
                            "online"
                        } else {
                            "offline"
                        },
                        tone: if m.me || m.online { Tone::Ok } else { Tone::Muted },
                        online: m.online || m.me,
                        me: m.me,
                    })
                    .collect()
            })
            .unwrap_or_default();
        for m in &g.members {
            let id = m.id.to_string();
            if rows.iter().any(|row| row.id == id) {
                continue;
            }
            let (status, tone) = match m.state {
                MemberState::Accepted => ("joined", Tone::Ok),
                MemberState::Sent => ("invited", Tone::Active),
                MemberState::Pending => ("waiting for them to be online", Tone::Warn),
                MemberState::Declined => ("declined", Tone::Error),
            };
            rows.push(MemberRow {
                id,
                name: m.name.clone(),
                status,
                tone,
                online: false,
                me: false,
            });
        }
        rows
    }

    /// One line for a moving (or queued) file: what it is doing, how far it is, and in which group.
    pub fn transfer_detail(&self, t: &TransferItem) -> String {
        let bytes = match t.total {
            Some(total) if total > 0 => {
                format!("{} of {}", format::size(t.done), format::size(total))
            }
            _ if t.done > 0 => format::size(t.done),
            _ => String::new(),
        };
        let mut parts = Vec::new();
        if t.queued {
            parts.push("Queued".to_owned());
            if !t.peer.is_empty() {
                parts.push(format!("from {}", t.peer));
            }
        } else {
            let action = match t.kind.as_str() {
                "upload" => "Uploading",
                "receive" => "Receiving from a guest",
                "prepare" => "Preparing to share",
                _ => "Downloading",
            };
            parts.push(if t.peer.is_empty() {
                action.to_owned()
            } else {
                let dir = if t.kind == "upload" { "to" } else { "from" };
                format!("{action} {dir} {}", t.peer)
            });
        }
        if !bytes.is_empty() {
            parts.push(bytes);
        }
        if let Some(g) = self.group(&t.group) {
            parts.push(group_title(g));
        }
        parts.join(" · ")
    }
}

/// How far a transfer is, 0 to 1; `None` while the size is not known (and for queued files).
pub fn transfer_progress(t: &TransferItem) -> Option<f32> {
    if t.queued {
        return None;
    }
    t.total
        .filter(|total| *total > 0)
        .map(|total| (t.done as f32 / total as f32).clamp(0., 1.))
}

/// A badge's colour.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tone {
    Ok,
    Active,
    Warn,
    Error,
    Muted,
}

#[derive(Clone, PartialEq)]
pub struct MemberRow {
    pub id: String,
    pub name: String,
    pub status: &'static str,
    pub tone: Tone,
    pub online: bool,
    pub me: bool,
}
