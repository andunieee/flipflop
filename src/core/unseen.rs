//! What happened in each group since the user last looked at it.

use iroh::EndpointId;

use super::{Core, GroupView};
use crate::state::Seen;

/// The news of one group. Empty news are not reported.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Unseen {
    pub group: String,
    /// The group's name (an implied group is named after the other device).
    pub title: String,
    /// An implied group: the other device.
    pub with: Option<EndpointId>,
    /// Files others offered: `(name, sender)`.
    pub files: Vec<(String, String)>,
    /// Texts others sent, oldest first: `(sender, text)`.
    pub texts: Vec<(String, String)>,
    /// The scratchpad's text changed.
    pub scratch: bool,
    /// Devices that joined.
    pub joined: Vec<String>,
}

impl Unseen {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.texts.is_empty() && !self.scratch && self.joined.is_empty()
    }

    /// The group tab that shows the first kind of news, in the order of `lines`.
    pub fn tab(&self) -> &'static str {
        if !self.files.is_empty() {
            "files"
        } else if !self.texts.is_empty() {
            "texts"
        } else if self.scratch {
            "pad"
        } else {
            "info"
        }
    }

    /// One line per kind of news, e.g. "2 new files: a.png, b.pdf".
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![];
        let plural = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        if !self.files.is_empty() {
            let names: Vec<&str> = self.files.iter().map(|f| f.0.as_str()).take(3).collect();
            let more = self.files.len().saturating_sub(3);
            lines.push(format!("{}: {}{}", plural(self.files.len(), "new file", "new files"), names.join(", "), if more > 0 { format!(" and {more} more") } else { String::new() }));
        }
        if let Some((from, text)) = self.texts.last() {
            let mut preview: String = text.lines().next().unwrap_or("").chars().take(60).collect();
            if preview.chars().count() < text.chars().count() {
                preview.push('…');
            }
            lines.push(format!("{}, latest from {from}: {preview}", plural(self.texts.len(), "new text", "new texts")));
        }
        if self.scratch {
            lines.push("the scratchpad changed".into());
        }
        if !self.joined.is_empty() {
            lines.push(format!("{} joined", self.joined.join(", ")));
        }
        lines
    }
}

pub(super) fn hash(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

/// What the view shows of the group now, as a `Seen`.
fn now(view: &GroupView) -> Seen {
    Seen {
        files: view.files.iter().map(|f| f.id.clone()).collect(),
        texts: view.texts.iter().map(|t| t.id.clone()).collect(),
        scratch: hash(&view.scratch),
        members: view.members.iter().filter(|m| !m.me).map(|m| m.id.clone()).collect(),
    }
}

/// The news in `view` compared with `seen`.
fn news(view: &GroupView, seen: &Seen, own_pad: Option<&str>) -> Unseen {
    let mut texts: Vec<(String, String)> =
        view.texts.iter().filter(|t| !t.mine && !seen.texts.contains(&t.id)).map(|t| (t.name.clone(), t.text.clone())).collect();
    texts.reverse(); // the view is newest first
    Unseen {
        group: view.id.clone(),
        files: view.files.iter().filter(|f| !f.mine && !f.stopped && !seen.files.contains(&f.id)).map(|f| (f.name.clone(), f.from_name.clone())).collect(),
        texts,
        scratch: !view.scratch.is_empty() && hash(&view.scratch) != seen.scratch && Some(hash(&view.scratch).as_str()) != own_pad,
        joined: view.members.iter().filter(|m| !m.me && !seen.members.contains(&m.id)).map(|m| m.name.clone()).collect(),
        ..Default::default()
    }
}

impl Core {
    /// The groups with news, implied groups included.
    pub fn unseen(&self) -> Vec<Unseen> {
        let seen: Vec<(String, Seen)> = self.0.store.lock().unwrap().state.groups.iter().map(|g| (g.id.clone(), g.seen.clone())).collect();
        let groups = self.groups().groups;
        seen.iter()
            .filter_map(|(gid, seen)| {
                let info = groups.iter().find(|g| g.id == *gid)?;
                let own_pad = self.0.sharing.own_pad.lock().unwrap().get(gid).cloned();
                let mut n = news(&self.group_view(gid)?, seen, own_pad.as_deref());
                n.with = info.with;
                n.title = match info.with.and_then(|w| info.members.iter().find(|m| m.id == w)) {
                    Some(m) => m.name.clone(),
                    None => info.alias.clone().unwrap_or_else(|| info.haiku.clone()),
                };
                (!n.is_empty()).then_some(n)
            })
            .collect()
    }

    /// Send the news to the UI if they differ from what it last got. Caller holds the publish lock.
    pub(super) fn emit_home(&self) {
        let news = self.unseen();
        let mut shown = self.0.sharing.home.lock().unwrap();
        if *shown != news {
            *shown = news.clone();
            drop(shown);
            self.emit(super::Event::Home(news));
        }
    }

    /// The user looked at this group: everything in it is now seen.
    pub fn mark_seen(&self, gid: &str) {
        let Some(view) = self.group_view(gid) else { return };
        let current = now(&view);
        let changed = self.0.store.lock().unwrap().state.groups.iter().any(|g| g.id == gid && g.seen != current);
        if changed {
            self.with_state(|st| {
                if let Some(g) = st.groups.iter_mut().find(|g| g.id == gid) {
                    g.seen = current;
                }
            });
        }
    }

    pub fn mark_all_seen(&self) {
        let ids: Vec<String> = self.0.store.lock().unwrap().state.groups.iter().map(|g| g.id.clone()).collect();
        for gid in ids {
            self.mark_seen(&gid);
        }
    }
}
