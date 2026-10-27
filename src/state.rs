//! Persistent app state: contacts, groups, invites. One JSON file, written atomically.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Contact {
    /// Endpoint id (hex).
    pub id: String,
    /// Last name the peer told us.
    pub name: Option<String>,
    /// Local rename.
    pub alias: Option<String>,
    /// We chose to pair with this peer (the "Add a peer" flow, or "Pair" in the peers menu).
    #[serde(default)]
    pub paired: bool,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemberState {
    /// Added, invite not delivered yet (peer offline).
    Pending,
    /// Invite delivered, no answer yet.
    Sent,
    Accepted,
    Declined,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Member {
    pub peer: String,
    pub state: MemberState,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Group {
    /// 16 random bytes (hex); the haiku name derives from it.
    pub id: String,
    pub alias: Option<String>,
    pub members: Vec<Member>,
    /// Download every file offered in the group as soon as it shows up.
    #[serde(default = "yes")]
    pub auto_download: bool,
    /// Stop sharing our files once every member has downloaded them.
    #[serde(default)]
    pub auto_stop: bool,
    /// Serve the files we download to the rest of the group too.
    #[serde(default = "yes")]
    pub auto_rehost: bool,
    /// The implied group of a pair of devices that paired with each other: the other device's id.
    /// Its id derives from both device ids, and it is not listed among the groups.
    #[serde(default)]
    pub with: Option<String>,
    /// What the user had seen of this group the last time they looked.
    #[serde(default)]
    pub seen: Seen,
    /// A guest group: anyone holding the link joins it from a browser.
    #[serde(default)]
    pub guest: Option<GuestLink>,
}

/// The secret behind a guest group's links, and what its guests may do.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct GuestLink {
    /// Part of every link: the only key.
    pub token: String,
    /// Guests may upload files.
    pub uploads: bool,
}

/// The state of a group at the last look, to tell what is new since.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    pub files: Vec<String>,
    pub texts: Vec<String>,
    /// blake3 of the scratchpad text.
    pub scratch: String,
    pub members: Vec<String>,
}

fn yes() -> bool {
    true
}

impl Group {
    pub fn new(id: String, members: Vec<Member>) -> Group {
        Group {
            id,
            alias: None,
            members,
            auto_download: true,
            auto_stop: false,
            auto_rehost: true,
            with: None,
            seen: Seen::default(),
            guest: None,
        }
    }
}

/// A file we offered to a group: where its bytes are, so we can serve them.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Share {
    pub group: String,
    pub file: String,
    pub path: PathBuf,
    /// We made the copy at `path` (Android pickers); it is deleted when sharing stops.
    #[serde(default)]
    pub staged: bool,
}

/// An invite we received and the user has not answered yet.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Invite {
    pub inviter: String,
    pub group: String,
}

/// An answer to an invite that has not reached the inviter yet.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Reply {
    pub peer: String,
    pub group: String,
    pub accept: bool,
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct State {
    pub contacts: Vec<Contact>,
    pub groups: Vec<Group>,
    pub invites: Vec<Invite>,
    /// `(inviter, group)` pairs the user declined: further invites for them are not shown.
    pub rejected: Vec<Invite>,
    pub replies: Vec<Reply>,
    /// Contacts the user deleted. They may still dial us (and we accept), but that does not save
    /// them again; connecting to them ourselves does.
    #[serde(default)]
    pub forgotten: Vec<String>,
    /// Peers that told us they paired with us.
    #[serde(default)]
    pub paired_us: Vec<String>,
    #[serde(default)]
    pub shares: Vec<Share>,
    /// Files we downloaded: where each one went.
    #[serde(default)]
    pub received: Vec<Share>,
}

pub struct Store {
    path: PathBuf,
    pub state: State,
}

impl Store {
    pub fn load(dir: &Path) -> Store {
        let path = dir.join("state.json");
        let state = std::fs::read(&path)
            .ok()
            .and_then(|b| {
                serde_json::from_slice(&b)
                    .map_err(|e| tracing::warn!("state.json unreadable: {e}"))
                    .ok()
            })
            .unwrap_or_default();
        Store { path, state }
    }

    pub fn save(&self) {
        let tmp = self.path.with_extension("json.tmp");
        let result = serde_json::to_vec_pretty(&self.state)
            .map_err(std::io::Error::other)
            .and_then(|b| std::fs::write(&tmp, b))
            .and_then(|_| std::fs::rename(&tmp, &self.path));
        if let Err(err) = result {
            tracing::warn!("could not save state: {err}");
        }
    }
}
