//! Listening along: someone playing one of the group's songs can share what they play, and the
//! others can play it with them, at the same moment. Nothing of it goes in the group document (every
//! pause would stay in its history): it travels as messages to the group's peers who are online, and
//! goes away with the connection.
//!
//! - [`Msg::Listen`]: what a sharer plays (or that they stopped sharing), sent on every change and
//!   every couple of seconds, so followers correct their drift and late comers catch up. What is not
//!   said again soon expires. Numbered: each goes on its own stream, and one overtaken by a later one
//!   is old news.
//! - [`Msg::Follow`]: a follower tells the sharer they listen along (or no longer do).
//! - [`Msg::Ping`] / [`Msg::Pong`]: how far each peer's clock is from ours, so a moment in a song
//!   ("at `at_ms` it was at `position_ms`") means the same on both sides. Times are on
//!   [`clock_ms`], which never jumps.

use std::{
    collections::{BTreeSet, HashMap},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use iroh::EndpointId;
use serde::{Deserialize, Serialize};

use super::{Core, Event, Msg, send_msg};
use crate::groupdoc::now_ms;

/// Clock samples kept per peer: the one with the shortest round trip says the most.
const SAMPLES: usize = 8;
/// How old the freshest clock sample of a sharer may get before we ask again.
const RESAMPLE_MS: i64 = 60_000;
/// How long what a sharer said holds without them saying it again (they do every couple of
/// seconds): a sharer gone quiet is no longer listed.
const EXPIRE: Duration = Duration::from_secs(5);
/// Pings when starting to listen along, for a good clock estimate from the start...
const BURST: usize = 5;
const BURST_GAP: Duration = Duration::from_millis(200);
/// ...and then now and then while listening along, as the clocks drift apart.
const REPING: Duration = Duration::from_secs(10);
/// A new clock estimate this far from the last one changes what followers aim at.
const OFFSET_CHANGED_MS: i64 = 5;

/// Our clock for listening along, in ms: the wall clock when the app started, run on by a
/// monotonic one, so it never jumps (the wall clock does, when the phone sets it), yet starts out
/// close to everyone else's.
pub fn clock_ms() -> i64 {
    static START: OnceLock<(i64, Instant)> = OnceLock::new();
    let (wall, at) = START.get_or_init(|| (now_ms(), Instant::now()));
    wall + at.elapsed().as_millis() as i64
}

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
    /// When, on the sender's [`clock_ms`] (as handed to the UI: on ours).
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
    /// What the others share, per group and sharer, on their clock, and when we heard it.
    heard: Mutex<HashMap<String, HashMap<EndpointId, (Listening, Instant)>>>,
    /// The number of the latest [`Msg::Listen`] heard per group and sharer, a stop's too.
    numbers: Mutex<HashMap<(String, EndpointId), u64>>,
    /// The number of our latest [`Msg::Listen`].
    number: AtomicU64,
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

impl Listen {
    /// Increasing, over restarts too (it starts from the time), so a peer that missed our going
    /// away does not take what we say next for old news.
    fn next_number(&self) -> u64 {
        self.number
            .fetch_max(now_ms().max(0) as u64 * 1000, Ordering::SeqCst);
        self.number.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Their clock minus ours, as best known; none before they answered a ping.
    fn offset(&self, peer: &EndpointId) -> Option<i64> {
        let clocks = self.clocks.lock().unwrap();
        clocks
            .get(peer)
            .filter(|s| !s.is_empty())
            .map(|s| best_offset(s))
    }
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
            number: listen.next_number(),
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
            self.keep_clock(gid, sharer);
        }
        self.send_soon(
            sharer,
            Msg::Follow {
                group: gid.to_owned(),
                on,
            },
        );
    }

    /// Who shares what they play in `gid` (on our clock), and who listens along with us there.
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
        let heard: Vec<(EndpointId, Listening)> = listen
            .heard
            .lock()
            .unwrap()
            .get(gid)
            .into_iter()
            .flatten()
            .map(|(id, (now, _))| (*id, now.clone()))
            .collect();
        let mut sharers: Vec<ListenView> = heard
            .into_iter()
            .map(|(id, now)| ListenView {
                sharer: id,
                name: name(&id),
                // With what we know of their clock now, not when they said it.
                now: Listening {
                    at_ms: now.at_ms - listen.offset(&id).unwrap_or(0),
                    ..now
                },
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
        self.send_soon(peer, Msg::Ping { t0: clock_ms() });
    }

    /// Listening along with `sharer` in `gid`: measure their clock a few times now, then now and
    /// then for as long as it lasts.
    fn keep_clock(&self, gid: &str, sharer: EndpointId) {
        let this = self.clone();
        let followed = Some((gid.to_owned(), sharer));
        tokio::spawn(async move {
            for n in 0.. {
                if *this.0.listen.following.lock().unwrap() != followed {
                    return;
                }
                this.ping(sharer);
                tokio::time::sleep(if n + 1 < BURST { BURST_GAP } else { REPING }).await;
            }
        });
    }

    pub(super) fn on_listen(
        &self,
        from: EndpointId,
        gid: String,
        now: Option<Listening>,
        number: u64,
    ) {
        if !self.has_group(&gid) || !self.group_peers(&gid).contains(&from) {
            return;
        }
        // Overtaken by a later one? (0: from a version that did not number them.)
        if number > 0 {
            let mut numbers = self.0.listen.numbers.lock().unwrap();
            let last = numbers.entry((gid.clone(), from)).or_default();
            if number <= *last {
                return;
            }
            *last = number;
        }
        let taken = {
            let clocks = self.0.listen.clocks.lock().unwrap();
            clocks
                .get(&from)
                .and_then(|s| s.iter().map(|s| s.taken).max())
        };
        if taken.is_none_or(|taken| clock_ms() - taken > RESAMPLE_MS) {
            self.ping(from);
        }
        {
            let mut heard = self.0.listen.heard.lock().unwrap();
            let group = heard.entry(gid.clone()).or_default();
            match now {
                Some(now) => {
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
        self.send_soon(from, Msg::Pong { t0, t1: clock_ms() });
    }

    pub(super) fn on_pong(&self, from: EndpointId, t0: i64, t1: i64) {
        let t3 = clock_ms();
        if t0 > t3 {
            return;
        }
        let listen = &self.0.listen;
        let before = listen.offset(&from);
        {
            let mut clocks = listen.clocks.lock().unwrap();
            let samples = clocks.entry(from).or_default();
            samples.push(sample(t0, t1, t3));
            if samples.len() > SAMPLES {
                samples.remove(0);
            }
        }
        let after = listen.offset(&from);
        if before
            .zip(after)
            .is_some_and(|(b, a)| (a - b).abs() < OFFSET_CHANGED_MS)
        {
            return;
        }
        // What they share is somewhere else on our clock now.
        let groups: Vec<String> = listen
            .heard
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, sharers)| sharers.contains_key(&from))
            .map(|(gid, _)| gid.clone())
            .collect();
        for gid in groups {
            self.emit_listening(&gid);
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
                let at = clock_ms();
                let now = Listening {
                    position_ms: now.position_at(at),
                    at_ms: at,
                    ..now
                };
                let number = self.0.listen.next_number();
                self.send_soon(
                    peer,
                    Msg::Listen {
                        group: gid,
                        now: Some(now),
                        number,
                    },
                );
            }
        }
        let following = self.0.listen.following.lock().unwrap().clone();
        if let Some((gid, sharer)) = following
            && sharer == peer
        {
            self.keep_clock(&gid, sharer);
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
        self.0
            .listen
            .numbers
            .lock()
            .unwrap()
            .retain(|(_, from), _| *from != peer);
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
    fn numbers_only_grow() {
        let listen = Listen::default();
        let first = listen.next_number();
        // From the time: above what a run before this one said.
        assert!(first > (now_ms() as u64 - 1_000) * 1000);
        assert!(listen.next_number() > first);
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
