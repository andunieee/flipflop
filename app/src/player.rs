//! What plays: this device's queue of songs and the player. The backend keeps it, not the UI, so the
//! queue goes on while no UI runs (Android with the screen off). One song plays at a time in the whole
//! app, whichever group it comes from. The UI sees [`MusicState`] (as [`crate::backend::Msg::Music`])
//! and changes it through the backend's `music_*` methods.
//!
//! Every song started gets a new ticket; what the player says about an older one is not news.

use std::{
    collections::BTreeMap,
    sync::{
        Mutex,
        atomic::{AtomicI64, AtomicU64, Ordering},
    },
    time::Duration,
};

use flipflop::{
    core::{FileView, ListenView, Listening},
    groupdoc::now_ms,
};
use iroh::EndpointId;

use crate::backend::Backend;

/// A song in the queue, or playing: an offered song, or one of an album's.
#[derive(Clone, PartialEq, Debug)]
pub struct Song {
    /// Tells apart the same song queued twice.
    pub key: u64,
    pub gid: String,
    pub fid: String,
    /// Which of the album's songs; 0 for a song on its own.
    pub track: usize,
    pub name: String,
    /// Who offers it (and the album it is from).
    pub from_name: String,
}

impl Song {
    pub fn is(&self, gid: &str, fid: &str, track: usize) -> bool {
        self.gid == gid && self.fid == fid && self.track == track
    }

    /// Of this offer (any of an album's songs).
    pub fn of(&self, gid: &str, fid: &str) -> bool {
        self.gid == gid && self.fid == fid
    }
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct MusicState {
    pub queue: Vec<Song>,
    pub current: Option<Song>,
    pub paused: bool,
    /// Waiting for the song to start.
    pub loading: bool,
    /// What we play is shared with its group, for the others to listen along.
    pub sharing: bool,
    /// Who listens along with what we share.
    pub listeners: Vec<String>,
    /// Whom we listen along with: what they play, we play.
    pub following: Option<Followed>,
    /// Who shares what they play, per group.
    pub sharers: BTreeMap<String, Vec<ListenView>>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Followed {
    pub gid: String,
    pub sharer: EndpointId,
    pub name: String,
}

/// How often a sharer says where it is, for followers to correct their drift (and what it says
/// expires without it).
const HEARTBEAT: Duration = Duration::from_secs(2);
/// How far from the sharer a follower may drift before it seeks.
const DRIFT_MS: i64 = 150;

/// What a player says about the song it was given.
#[derive(Debug)]
pub enum Update {
    Paused(bool),
    /// It started.
    Loaded,
    /// It played to its end.
    Ended,
    Failed(String),
}

/// The platform's player: libmpv on the desktop, Android's own (in `MusicService`, which keeps the
/// app playing with the screen off) on Android.
#[derive(Clone)]
enum Player {
    #[cfg(not(target_os = "android"))]
    Mpv(crate::mpv::Mpv),
    #[cfg(target_os = "android")]
    Android,
}

impl Player {
    fn start(backend: &Backend) -> Result<Player, String> {
        #[cfg(not(target_os = "android"))]
        {
            let backend = backend.clone();
            crate::mpv::Mpv::start(Box::new(move |ticket, update| {
                backend.music().update(&backend, ticket, update)
            }))
            .map(Player::Mpv)
        }
        #[cfg(target_os = "android")]
        {
            // It tells through the Activity's events (see `Backend::on_activity`).
            let _ = backend;
            Ok(Player::Android)
        }
    }

    fn load(&self, ticket: u64, url: String, song: &Song, start_ms: u64) {
        match self {
            #[cfg(not(target_os = "android"))]
            Player::Mpv(mpv) => {
                let _ = song;
                mpv.load(ticket, url, start_ms)
            }
            #[cfg(target_os = "android")]
            Player::Android => {
                flipflop::platform::music_play(ticket, &url, &song.name, &song.from_name, start_ms)
            }
        }
    }

    /// Where the song plays, in ms; none before it plays.
    fn position(&self) -> Option<u64> {
        match self {
            #[cfg(not(target_os = "android"))]
            Player::Mpv(mpv) => mpv.position(),
            #[cfg(target_os = "android")]
            Player::Android => flipflop::platform::music_position(),
        }
    }

    fn seek(&self, ms: u64) {
        match self {
            #[cfg(not(target_os = "android"))]
            Player::Mpv(mpv) => mpv.seek(ms),
            #[cfg(target_os = "android")]
            Player::Android => flipflop::platform::music_seek(ms),
        }
    }

    fn pause(&self, paused: bool) {
        match self {
            #[cfg(not(target_os = "android"))]
            Player::Mpv(mpv) => mpv.pause(paused),
            #[cfg(target_os = "android")]
            Player::Android => flipflop::platform::music_pause(paused),
        }
    }

    fn stop(&self) {
        match self {
            #[cfg(not(target_os = "android"))]
            Player::Mpv(mpv) => mpv.stop(),
            #[cfg(target_os = "android")]
            Player::Android => flipflop::platform::music_stop(),
        }
    }
}

#[derive(Default)]
pub struct Music {
    state: Mutex<MusicState>,
    /// Started the first time something plays; why not, if it cannot be.
    player: Mutex<Option<Result<Player, String>>>,
    keys: AtomicU64,
    /// The playing song's.
    ticket: AtomicU64,
    /// The group what we play is shared with, and what it was last told.
    shared: Mutex<Option<(String, Option<Listening>)>>,
    /// Counts sharing turned on, so only the latest heartbeat goes on.
    heartbeats: AtomicU64,
    /// How much later than asked a follower's seek lands, learned from its drift.
    lead_ms: AtomicI64,
}

impl Music {
    pub fn state(&self) -> MusicState {
        self.state.lock().unwrap().clone()
    }

    /// Change the state, and tell the UI.
    fn change<R>(&self, backend: &Backend, f: impl FnOnce(&mut MusicState) -> R) -> R {
        let (r, now) = {
            let mut state = self.state.lock().unwrap();
            let r = f(&mut state);
            (r, state.clone())
        };
        backend.send_music(now);
        self.announce(backend, false);
        r
    }

    fn player(&self, backend: &Backend) -> Option<Player> {
        let mut player = self.player.lock().unwrap();
        match player.get_or_insert_with(|| Player::start(backend)) {
            Ok(player) => Some(player.clone()),
            Err(err) => {
                backend.notice(err.clone());
                None
            }
        }
    }

    /// The songs of an offer: the one it is, or an album's, in order.
    pub fn songs(&self, gid: &str, f: &FileView) -> Vec<Song> {
        let from = if f.mine {
            "you".to_owned()
        } else {
            f.from_name.clone()
        };
        let song = |track: usize, name: String, from_name: String| Song {
            key: self.keys.fetch_add(1, Ordering::Relaxed) + 1,
            gid: gid.to_owned(),
            fid: f.id.clone(),
            track,
            name,
            from_name,
        };
        if f.tracks.is_empty() {
            return vec![song(0, f.name.clone(), from)];
        }
        f.tracks
            .iter()
            .enumerate()
            .map(|(i, path)| {
                let name = path.rsplit('/').next().unwrap_or(path).to_owned();
                song(i, name, format!("{} · {from}", f.name))
            })
            .collect()
    }

    /// Play `song` now, in place of what plays.
    pub fn play(&self, backend: &Backend, song: Song) {
        self.play_from(backend, song, None)
    }

    /// Play `song` now; listening along, from where `along` says it is once the song can start.
    fn play_from(&self, backend: &Backend, song: Song, along: Option<Listening>) {
        let Some(player) = self.player(backend) else {
            return;
        };
        let ticket = self.ticket.fetch_add(1, Ordering::SeqCst) + 1;
        self.change(backend, |s| {
            s.current = Some(song.clone());
            s.paused = false;
            s.loading = true;
        });
        let Some(core) = backend.core() else { return };
        let backend = backend.clone();
        backend.clone().spawn_task(async move {
            let music = backend.music();
            match core.music_source(&song.gid, &song.fid, song.track).await {
                // Unless something else was asked for meanwhile.
                Ok(url) if music.ticket.load(Ordering::SeqCst) == ticket => {
                    // Worked out now, not when asked: finding the song takes a moment.
                    let start = along.as_ref().map_or(0, |l| {
                        l.position_at(now_ms())
                            + music.lead_ms.load(Ordering::Relaxed).max(0) as u64
                    });
                    player.load(ticket, url, &song, start);
                    // The sharer paused: so do we, once the song is there.
                    if along.is_some_and(|l| l.paused) {
                        player.pause(true);
                    }
                }
                Ok(_) => {}
                Err(err) => music.update(&backend, ticket, Update::Failed(format!("{err:#}"))),
            }
        });
    }

    /// A song's play/pause button (an album's `track`): pause or resume it if it is the one
    /// playing, else play it now.
    pub fn press(&self, backend: &Backend, gid: &str, f: &FileView, track: usize) {
        self.stop_following(backend);
        let current = self
            .state
            .lock()
            .unwrap()
            .current
            .as_ref()
            .is_some_and(|c| c.is(gid, &f.id, track));
        if current {
            return self.toggle(backend);
        }
        if let Some(song) = self.songs(gid, f).into_iter().nth(track) {
            self.play(backend, song);
        }
    }

    /// An album's play/pause button: pause or resume it if one of its songs plays, else play it
    /// from its first song, the others next.
    pub fn press_album(&self, backend: &Backend, gid: &str, f: &FileView) {
        self.stop_following(backend);
        let current = self
            .state
            .lock()
            .unwrap()
            .current
            .as_ref()
            .is_some_and(|c| c.of(gid, &f.id));
        if current {
            return self.toggle(backend);
        }
        let mut songs = self.songs(gid, f).into_iter();
        let Some(first) = songs.next() else { return };
        self.change(backend, |s| {
            s.queue.splice(0..0, songs);
        });
        self.play(backend, first);
    }

    /// Pause or resume; with nothing playing, start the queue.
    pub fn toggle(&self, backend: &Backend) {
        self.stop_following(backend);
        if self.state.lock().unwrap().current.is_none() {
            return self.next(backend);
        }
        let Some(player) = self.player(backend) else {
            return;
        };
        let paused = self.change(backend, |s| {
            s.paused = !s.paused;
            s.paused
        });
        player.pause(paused);
    }

    /// Play the first in the queue, or stop if it is empty.
    pub fn next(&self, backend: &Backend) {
        self.stop_following(backend);
        let first = self.change(backend, |s| {
            (!s.queue.is_empty()).then(|| s.queue.remove(0))
        });
        match first {
            Some(song) => self.play(backend, song),
            None => self.stop(backend),
        }
    }

    fn stop(&self, backend: &Backend) {
        self.ticket.fetch_add(1, Ordering::SeqCst);
        self.change(backend, |s| {
            s.current = None;
            s.paused = false;
            s.loading = false;
        });
        if let Some(Ok(player)) = self.player.lock().unwrap().as_ref() {
            player.stop();
        }
    }

    /// Empty the queue and stop playing.
    pub fn clear(&self, backend: &Backend) {
        self.stop_following(backend);
        self.change(backend, |s| s.queue.clear());
        self.stop(backend);
    }

    pub fn enqueue(&self, backend: &Backend, songs: Vec<Song>) {
        self.change(backend, |s| s.queue.extend(songs));
    }

    pub fn remove(&self, backend: &Backend, key: u64) {
        self.change(backend, |s| s.queue.retain(|e| e.key != key));
    }

    /// Put the song `key` where `before` is (at the end without one).
    pub fn reorder(&self, backend: &Backend, key: u64, before: Option<u64>) {
        if before == Some(key) {
            return;
        }
        self.change(backend, |s| {
            let Some(from) = s.queue.iter().position(|e| e.key == key) else {
                return;
            };
            let song = s.queue.remove(from);
            let to = before
                .and_then(|b| s.queue.iter().position(|e| e.key == b))
                .unwrap_or(s.queue.len());
            s.queue.insert(to, song);
        });
    }

    /// What the player says about the song with `ticket`.
    pub fn update(&self, backend: &Backend, ticket: u64, update: Update) {
        if ticket != self.ticket.load(Ordering::SeqCst) {
            return;
        }
        match update {
            Update::Paused(paused) => self.change(backend, |s| s.paused = paused),
            Update::Loaded => {
                self.change(backend, |s| s.loading = false);
                // The song took a moment to start: catch up once it plays.
                if self.state.lock().unwrap().following.is_some() {
                    let backend = backend.clone();
                    backend.clone().spawn_task(async move {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        backend.music().resync(&backend);
                    });
                }
            }
            // Listening along, the sharer says what comes next.
            Update::Ended if self.state.lock().unwrap().following.is_some() => {}
            Update::Ended => self.next(backend),
            Update::Failed(why) => {
                let name = self
                    .state
                    .lock()
                    .unwrap()
                    .current
                    .as_ref()
                    .map(|s| s.name.clone());
                backend.notice(match name {
                    Some(name) => format!("{name}: {why}"),
                    None => why,
                });
                self.next(backend);
            }
        }
    }
}

// ---- listening along ---------------------------------------------------------------------------

impl Music {
    /// Share what we play with its group (and each next song's), or stop.
    pub fn set_sharing(&self, backend: &Backend, on: bool) {
        self.change(backend, |s| {
            s.sharing = on;
            if !on {
                s.listeners.clear();
            }
        });
        if !on {
            return;
        }
        let beat = self.heartbeats.fetch_add(1, Ordering::SeqCst) + 1;
        let backend = backend.clone();
        backend.clone().spawn_task(async move {
            loop {
                tokio::time::sleep(HEARTBEAT).await;
                let music = backend.music();
                if music.heartbeats.load(Ordering::SeqCst) != beat
                    || !music.state.lock().unwrap().sharing
                {
                    return;
                }
                music.announce(&backend, true);
            }
        });
    }

    /// Tell the group what we play, if we share it and it changed (or, `again`, anyway).
    fn announce(&self, backend: &Backend, again: bool) {
        let now = self.state.lock().unwrap().clone();
        let at = now_ms();
        let told = self
            .shared
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|(_, l)| l.clone());
        let mut unknown = false;
        // A follower plays what someone else shares: nothing to tell.
        let playing = now
            .current
            .as_ref()
            .filter(|_| now.sharing && now.following.is_none())
            .map(|song| {
                let position = if now.loading {
                    Some(0)
                } else {
                    self.player
                        .lock()
                        .unwrap()
                        .as_ref()
                        .and_then(|p| p.as_ref().ok()?.position())
                        .or_else(|| {
                            // The player cannot say (Android without its Activity): from what we last said.
                            told.as_ref()
                                .filter(|l| (&l.fid, l.track) == (&song.fid, song.track))
                                .map(|l| l.position_at(at))
                        })
                };
                unknown = position.is_none();
                (
                    song.gid.clone(),
                    Listening {
                        fid: song.fid.clone(),
                        track: song.track,
                        name: song.name.clone(),
                        from_name: song.from_name.clone(),
                        position_ms: position.unwrap_or(0),
                        at_ms: at,
                        // Not running ahead of a song still starting.
                        paused: now.paused || now.loading,
                    },
                )
            });
        // Better nothing than a wrong position, which would send every follower back.
        if unknown {
            return;
        }
        // Decided under the lock, told after it: telling the core can come back here.
        let (gone, tell) = {
            let mut shared = self.shared.lock().unwrap();
            // Another group's song, or none: that group no longer hears from us.
            let gone = match shared.as_ref() {
                Some((gid, _)) if playing.as_ref().is_none_or(|(g, _)| g != gid) => {
                    shared.take().map(|(gid, _)| gid)
                }
                _ => None,
            };
            let tell = playing.filter(|(_, listening)| {
                let same = shared
                    .as_ref()
                    .and_then(|(_, l)| l.as_ref())
                    .is_some_and(|l| {
                        (&l.fid, l.track, l.paused)
                            == (&listening.fid, listening.track, listening.paused)
                    });
                again || !same
            });
            if let Some((gid, listening)) = &tell {
                *shared = Some((gid.clone(), Some(listening.clone())));
            }
            (gone, tell)
        };
        // On the runtime: what the core sends goes out from it.
        backend.run(|core| {
            if let Some(gid) = gone {
                core.share_listen(&gid, None);
            }
            if let Some((gid, listening)) = tell {
                core.share_listen(&gid, Some(listening));
            }
            Ok(())
        });
    }

    /// What the core says about who shares and who listens along in `gid`.
    pub fn heard(
        &self,
        backend: &Backend,
        gid: String,
        sharers: Vec<ListenView>,
        followers: Vec<String>,
    ) {
        let mine = self
            .shared
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|(g, _)| *g == gid);
        let followed = self
            .state
            .lock()
            .unwrap()
            .following
            .clone()
            .filter(|f| f.gid == gid);
        let along = followed
            .as_ref()
            .and_then(|f| sharers.iter().find(|v| v.sharer == f.sharer))
            .cloned();
        self.change(backend, |s| {
            if mine {
                s.listeners = followers;
            }
            if sharers.is_empty() {
                s.sharers.remove(&gid);
            } else {
                s.sharers.insert(gid.clone(), sharers);
            }
            if followed.is_some() && along.is_none() {
                s.following = None;
            }
        });
        match (followed, along) {
            (Some(f), None) => backend.notice(format!("{} stopped sharing", f.name)),
            (Some(_), Some(v)) => self.apply(backend, &gid, v.now),
            _ => {}
        }
    }

    /// Play what `sharer` plays in `gid`, from where they are, and go on doing so.
    pub fn listen_along(&self, backend: &Backend, gid: &str, sharer: EndpointId) {
        let Some(view) = self
            .state
            .lock()
            .unwrap()
            .sharers
            .get(gid)
            .and_then(|v| v.iter().find(|v| v.sharer == sharer))
            .cloned()
        else {
            return;
        };
        self.stop_following(backend);
        backend.run(|core| Ok(core.follow(gid, sharer, true)));
        self.change(backend, |s| {
            s.following = Some(Followed {
                gid: gid.to_owned(),
                sharer,
                name: view.name.clone(),
            })
        });
        self.apply(backend, gid, view.now);
    }

    /// No longer listen along; what plays goes on, on its own.
    pub fn stop_following(&self, backend: &Backend) {
        let Some(f) = self.change(backend, |s| s.following.take()) else {
            return;
        };
        backend.run(|core| Ok(core.follow(&f.gid, f.sharer, false)));
    }

    /// Make what plays match what the followed sharer plays.
    fn apply(&self, backend: &Backend, gid: &str, along: Listening) {
        let now = self.state.lock().unwrap().clone();
        let same = now
            .current
            .as_ref()
            .is_some_and(|c| c.is(gid, &along.fid, along.track));
        if !same {
            let song = Song {
                key: self.keys.fetch_add(1, Ordering::Relaxed) + 1,
                gid: gid.to_owned(),
                fid: along.fid.clone(),
                track: along.track,
                name: along.name.clone(),
                from_name: along.from_name.clone(),
            };
            let paused = along.paused;
            self.play_from(backend, song, Some(along));
            if paused {
                self.change(backend, |s| s.paused = true);
            }
            return;
        }
        if now.paused != along.paused {
            self.set_paused(backend, along.paused);
        }
        if !now.loading {
            self.seek_to(&along);
        }
    }

    fn set_paused(&self, backend: &Backend, paused: bool) {
        let Some(player) = self.player(backend) else {
            return;
        };
        self.change(backend, |s| s.paused = paused);
        player.pause(paused);
    }

    /// Seek to where the sharer is, if we drifted from it.
    fn seek_to(&self, along: &Listening) {
        let Some(Ok(player)) = self.player.lock().unwrap().clone() else {
            return;
        };
        let Some(position) = player.position() else {
            return;
        };
        let expected = along.position_at(now_ms()) as i64;
        let drift = expected - position as i64;
        if drift.abs() <= DRIFT_MS {
            return;
        }
        // Still behind after a seek: seeks land later than asked, by about that much.
        let lead = (self.lead_ms.load(Ordering::Relaxed) + drift / 2).clamp(0, 2_000);
        self.lead_ms.store(lead, Ordering::Relaxed);
        tracing::info!("listening along: {drift} ms off, seeking (lead {lead} ms)");
        player.seek((expected + lead).max(0) as u64);
    }

    /// Check the drift against what the followed sharer last said.
    fn resync(&self, backend: &Backend) {
        let now = self.state.lock().unwrap().clone();
        let Some(f) = now.following else { return };
        let along = now
            .sharers
            .get(&f.gid)
            .and_then(|v| v.iter().find(|v| v.sharer == f.sharer));
        if let Some(v) = along {
            let _ = backend;
            self.seek_to(&v.now);
        }
    }
}
