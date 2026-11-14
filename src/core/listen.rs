//! Listening along: someone playing one of the group's songs can share what they play, and the
//! others can play it with them, at the same moment. Nothing of it goes in the group document (every
//! pause would stay in its history): it travels as messages to the group's peers who are online, and
//! goes away with the connection.
//!
//! - [`Msg::Listen`]: what a sharer plays (or that they stopped sharing), sent on every change and
//!   now and then while it plays, so followers correct their drift and late comers catch up.
//! - [`Msg::Follow`]: a follower tells the sharer they listen along (or no longer do).
//! - [`Msg::Ping`] / [`Msg::Pong`]: how far each peer's clock is from ours, so a moment in a song
//!   ("at `at_ms` it was at `position_ms`") means the same on both sides.

use std::{
    collections::{BTreeSet, HashMap},
    sync::Mutex,
    time::{Duration, Instant},
};

use iroh::EndpointId;
use serde::{Deserialize, Serialize};

use super::{Core, Event, Msg, send_msg};
use crate::groupdoc::now_ms;

/// Clock samples kept per peer: the one with the shortest round trip says the most.
const SAMPLES: usize = 8;
/// How old the freshest clock sample may get while listening along before we ask again.
const RESAMPLE_MS: i64 = 60_000;
/// How long what a sharer said holds without them saying it again (they do every couple of
/// seconds): a sharer gone quiet is no longer listed.
const EXPIRE: Duration = Duration::from_secs(5);

/// What a sharer plays.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Listening {
    pub fid: String,
    /// Which of an album's songs; 0 for a song on its own.
    pub track: usize,
    pub name: String,
    /// Who offers it (and the album it is from).
    pub from_name: String,
    /// Where in the song it was at `at_ms`.
    pub position_ms: u64,
    /// When, on the sender's clock (once received: on ours).
    pub at_ms: i64,
    pub paused: bool,
}

impl Listening {
    /// Where in the song it is at `now` (our clock).
    pub fn position_at(&self, now: i64) -> u64 {
        if self.paused {
            self.position_ms
        } else {
            (self.position_ms as i64 + (now - self.at_ms).max(0)) as u64
        }
    }
}

/// Someone else sharing what they play.
#[derive(Debug, Clone, PartialEq)]
pub struct ListenView {
    pub sharer: EndpointId,
    pub name: String,
    pub now: Listening,
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    rtt: i64,
    /// Their clock minus ours.
    offset: i64,
    taken: i64,
}

#[derive(Default)]
pub(super) struct Listen {
    /// What we share, per group.
    mine: Mutex<HashMap<String, Listening>>,
    /// What the others share, per group and sharer, on our clock, and when we heard it.
    heard: Mutex<HashMap<String, HashMap<EndpointId, (Listening, Instant)>>>,
    /// Who listens along with us, per group.
    followers: Mutex<HashMap<String, BTreeSet<EndpointId>>>,
    /// Whom we listen along with: told again when they reconnect.
    following: Mutex<Option<(String, EndpointId)>>,
    clocks: Mutex<HashMap<EndpointId, Vec<Sample>>>,
}

/// From a ping sent at `t0`, answered at `t1` (their clock) and back at `t3`.
fn sample(t0: i64, t1: i64, t3: i64) -> Sample {
    Sample {
        rtt: t3 - t0,
        offset: t1 - (t0 + t3) / 2,
        taken: t3,
    }
}

/// Their clock minus ours, from the sample with the shortest round trip.
fn best_offset(samples: &[Sample]) -> i64 {
    samples.iter().min_by_key(|s| s.rtt).map_or(0, |s| s.offset)
}

impl Core {
    /// Share what we play in `gid` with the group, or (`None`) stop sharing.
    pub fn share_listen(&self, gid: &str, now: Option<Listening>) {
        if !self.has_group(gid) {
            return;
        }
        let listen = &self.0.listen;
        match &now {
            Some(now) => {
                listen
                    .mine
                    .lock()
                    .unwrap()
                    .insert(gid.to_owned(), now.clone());
            }
            None => {
                let was = listen.mine.lock().unwrap().remove(gid).is_some();
                listen.followers.lock().unwrap().remove(gid);
                if !was {
                    return;
                }
                self.emit_listening(gid);
            }
        }
        let msg = Msg::Listen {
            group: gid.to_owned(),
            now,
        };
        for peer in self.group_peers(gid) {
            self.send_soon(peer, msg.clone());
        }
    }

    /// Listen along with `sharer` in `gid`, or no longer.
    pub fn follow(&self, gid: &str, sharer: EndpointId, on: bool) {
        {
            let mut following = self.0.listen.following.lock().unwrap();
            if on {
                *following = Some((gid.to_owned(), sharer));
            } else if following.as_ref() == Some(&(gid.to_owned(), sharer)) {
                *following = None;
            }
        }
        if on {
            self.ping(sharer);
        }
        self.send_soon(
            sharer,
            Msg::Follow {
                group: gid.to_owned(),
                on,
            },
        );
    }

    /// Who shares what they play in `gid`, and who listens along with us there.
    pub fn listening(&self, gid: &str) -> (Vec<ListenView>, Vec<String>) {
        let names: HashMap<String, String> =
            self.with_doc(gid, |d| d.members()).into_iter().collect();
        let name = |id: &EndpointId| {
            names
                .get(&id.to_string())
                .cloned()
                .unwrap_or_else(|| id.fmt_short().to_string())
        };
        let listen = &self.0.listen;
        let mut sharers: Vec<ListenView> = listen
            .heard
            .lock()
            .unwrap()
            .get(gid)
            .into_iter()
            .flatten()
            .map(|(id, (now, _))| ListenView {
                sharer: *id,
                name: name(id),
                now: now.clone(),
            })
            .collect();
        sharers.sort_by(|a, b| a.name.cmp(&b.name));
        let followers = listen
            .followers
            .lock()
            .unwrap()
            .get(gid)
            .into_iter()
            .flatten()
            .map(name)
            .collect();
        (sharers, followers)
    }

    fn emit_listening(&self, gid: &str) {
        let (sharers, followers) = self.listening(gid);
        self.emit(Event::Listening {
            group: gid.to_owned(),
            sharers,
            followers,
        });
    }

    fn send_soon(&self, peer: EndpointId, msg: Msg) {
        let Some(conn) = self.connection(peer) else {
            return;
        };
        tokio::spawn(async move {
            if let Err(err) = send_msg(&conn, &msg).await {
                tracing::debug!("listen message to {} failed: {err:#}", peer.fmt_short());
            }
        });
    }

    fn ping(&self, peer: EndpointId) {
        self.send_soon(peer, Msg::Ping { t0: now_ms() });
    }

    pub(super) fn on_listen(&self, from: EndpointId, gid: String, now: Option<Listening>) {
        if !self.has_group(&gid) || !self.group_peers(&gid).contains(&from) {
            return;
        }
        let clock = {
            let clocks = self.0.listen.clocks.lock().unwrap();
            clocks
                .get(&from)
                .map(|s| (best_offset(s), s.iter().map(|s| s.taken).max().unwrap_or(0)))
        };
        if clock.is_none_or(|(_, taken)| now_ms() - taken > RESAMPLE_MS) {
            self.ping(from);
        }
        {
            let mut heard = self.0.listen.heard.lock().unwrap();
            let group = heard.entry(gid.clone()).or_default();
            match now {
                Some(mut now) => {
                    now.at_ms -= clock.map_or(0, |(offset, _)| offset);
                    group.insert(from, (now, Instant::now()));
                    let this = self.clone();
                    let gid = gid.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(EXPIRE).await;
                        this.expire(&gid, from);
                    });
                }
                None => {
                    group.remove(&from);
                }
            }
        }
        self.emit_listening(&gid);
    }

    /// Forget what `from` shares in `gid` if they did not say it again in time.
    fn expire(&self, gid: &str, from: EndpointId) {
        let gone = {
            let mut heard = self.0.listen.heard.lock().unwrap();
            let Some(group) = heard.get_mut(gid) else {
                return;
            };
            let stale = group
                .get(&from)
                .is_some_and(|(_, at)| at.elapsed() >= EXPIRE);
            if stale {
                group.remove(&from);
            }
            stale
        };
        if gone {
            self.emit_listening(gid);
        }
    }

    pub(super) fn on_follow(&self, from: EndpointId, gid: String, on: bool) {
        if !self.0.listen.mine.lock().unwrap().contains_key(&gid)
            || !self.group_peers(&gid).contains(&from)
        {
            return;
        }
        {
            let mut followers = self.0.listen.followers.lock().unwrap();
            let set = followers.entry(gid.clone()).or_default();
            if on {
                set.insert(from)
            } else {
                set.remove(&from)
            };
        }
        self.emit_listening(&gid);
    }

    pub(super) fn on_ping(&self, from: EndpointId, t0: i64) {
        self.send_soon(from, Msg::Pong { t0, t1: now_ms() });
    }

    pub(super) fn on_pong(&self, from: EndpointId, t0: i64, t1: i64) {
        let t3 = now_ms();
        if t0 > t3 {
            return;
        }
        let mut clocks = self.0.listen.clocks.lock().unwrap();
        let samples = clocks.entry(from).or_default();
        samples.push(sample(t0, t1, t3));
        if samples.len() > SAMPLES {
            samples.remove(0);
        }
    }

    /// A peer connected: measure its clock, and tell it what we share and whom we follow.
    pub(super) fn listen_connected(&self, peer: EndpointId) {
        self.ping(peer);
        let mine: Vec<(String, Listening)> = self
            .0
            .listen
            .mine
            .lock()
            .unwrap()
            .iter()
            .map(|(g, l)| (g.clone(), l.clone()))
            .collect();
        for (gid, now) in mine {
            if self.group_peers(&gid).contains(&peer) {
                // Where it is now, not where it was when last told.
                let at = now_ms();
                let now = Listening {
                    position_ms: now.position_at(at),
                    at_ms: at,
                    ..now
                };
                self.send_soon(
                    peer,
                    Msg::Listen {
                        group: gid,
                        now: Some(now),
                    },
                );
            }
        }
        let following = self.0.listen.following.lock().unwrap().clone();
        if let Some((gid, sharer)) = following
            && sharer == peer
        {
            self.send_soon(
                peer,
                Msg::Follow {
                    group: gid,
                    on: true,
                },
            );
        }
    }

    /// A peer went away: so did what it shared, and it no longer listens along.
    pub(super) fn listen_gone(&self, peer: EndpointId) {
        let mut touched = BTreeSet::new();
        for (gid, sharers) in self.0.listen.heard.lock().unwrap().iter_mut() {
            if sharers.remove(&peer).is_some() {
                touched.insert(gid.clone());
            }
        }
        for (gid, set) in self.0.listen.followers.lock().unwrap().iter_mut() {
            if set.remove(&peer) {
                touched.insert(gid.clone());
            }
        }
        self.0.listen.clocks.lock().unwrap().remove(&peer);
        for gid in touched {
            self.emit_listening(&gid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_offset() {
        // Their clock is 500 ms ahead; 40 ms each way.
        let s = sample(1_000, 1_540, 1_080);
        assert_eq!((s.rtt, s.offset), (80, 500));
        // A slow round trip says less than a quick one.
        let slow = sample(2_000, 2_900, 2_600);
        assert_eq!(best_offset(&[slow, s]), 500);
        assert_eq!(best_offset(&[]), 0);
    }

    #[test]
    fn position() {
        let l = Listening {
            fid: "f".into(),
            track: 0,
            name: "n".into(),
            from_name: "a".into(),
            position_ms: 10_000,
            at_ms: 50_000,
            paused: false,
        };
        assert_eq!(l.position_at(52_500), 12_500);
        // Not before it was said.
        assert_eq!(l.position_at(49_000), 10_000);
        let paused = Listening { paused: true, ..l };
        assert_eq!(paused.position_at(60_000), 10_000);
    }
}
