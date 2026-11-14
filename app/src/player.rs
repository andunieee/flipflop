//! What plays: this device's queue of songs and the player. The backend keeps it, not the UI, so the
//! queue goes on while no UI runs (Android with the screen off). One song plays at a time in the whole
//! app, whichever group it comes from. The UI sees [`MusicState`] (as [`crate::backend::Msg::Music`])
//! and changes it through the backend's `music_*` methods.
//!
//! Every song started gets a new ticket; what the player says about an older one is not news.

use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

use flipflop::core::FileView;

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
}

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

    fn load(&self, ticket: u64, url: String, song: &Song) {
        match self {
            #[cfg(not(target_os = "android"))]
            Player::Mpv(mpv) => {
                let _ = song;
                mpv.load(ticket, url)
            }
            #[cfg(target_os = "android")]
            Player::Android => flipflop::platform::music_play(ticket, &url, &song.name, &song.from_name),
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
        let from = if f.mine { "you".to_owned() } else { f.from_name.clone() };
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
        let Some(player) = self.player(backend) else { return };
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
                    player.load(ticket, url, &song)
                }
                Ok(_) => {}
                Err(err) => music.update(&backend, ticket, Update::Failed(format!("{err:#}"))),
            }
        });
    }

    /// A song's play/pause button (an album's `track`): pause or resume it if it is the one
    /// playing, else play it now.
    pub fn press(&self, backend: &Backend, gid: &str, f: &FileView, track: usize) {
        let current = self.state.lock().unwrap().current.as_ref().is_some_and(|c| c.is(gid, &f.id, track));
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
        let current = self.state.lock().unwrap().current.as_ref().is_some_and(|c| c.of(gid, &f.id));
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
        if self.state.lock().unwrap().current.is_none() {
            return self.next(backend);
        }
        let Some(player) = self.player(backend) else { return };
        let paused = self.change(backend, |s| {
            s.paused = !s.paused;
            s.paused
        });
        player.pause(paused);
    }

    /// Play the first in the queue, or stop if it is empty.
    pub fn next(&self, backend: &Backend) {
        let first = self.change(backend, |s| (!s.queue.is_empty()).then(|| s.queue.remove(0)));
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
            let Some(from) = s.queue.iter().position(|e| e.key == key) else { return };
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
            Update::Loaded => self.change(backend, |s| s.loading = false),
            Update::Ended => self.next(backend),
            Update::Failed(why) => {
                let name = self.state.lock().unwrap().current.as_ref().map(|s| s.name.clone());
                backend.notice(match name {
                    Some(name) => format!("{name}: {why}"),
                    None => why,
                });
                self.next(backend);
            }
        }
    }
}
