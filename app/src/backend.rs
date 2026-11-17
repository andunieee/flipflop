//! The node behind the UI: starts [`Core`] in the background and runs the commands that take a while.
//! Everything the core reports reaches the UI as a [`Msg`] on the channel [`Backend::attach`] returns.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use flipflop::{
    core::{Core, Event},
    format,
};
use iroh::EndpointId;
use tokio::{
    runtime::Handle,
    sync::{mpsc, watch},
};

#[cfg(target_os = "android")]
use crate::player::Update;
use crate::player::{Music, MusicState};

/// What the UI learns from the node (and from the Activity, on Android).
pub enum Msg {
    Core(Event),
    /// The node is up.
    Ready {
        core: Core,
        download_dir: String,
    },
    BleStatus(String),
    Notice(String),
    /// Go to the peers page (a ticket just connected).
    ShowPeers,
    /// The webcam is (or is no longer) looking for a code.
    Scanning(bool),
    /// A wasm app was read: show its form.
    AppDraft {
        gid: String,
        draft: flipflop::core::AppDraft,
    },
    /// Another app shared a text and/or files (Android).
    Shared {
        text: String,
        files: u32,
    },
    /// Room taken by the system bars and the keyboard, and whether the keyboard is up (Android).
    Insets {
        insets: [f32; 4],
        keyboard: bool,
    },
    /// What plays, and what plays next.
    Music(MusicState),
    /// Something the Android IME sent to the focused field (the scratchpad).
    Ime(crate::model::Ime),
}

/// A webcam preview frame: width, height, RGBA.
pub type Frame = Option<(u32, u32, bytes::Bytes)>;

/// How long typing in a field must pause before what was typed is saved.
pub(crate) const TYPING_PAUSE: Duration = Duration::from_millis(400);

/// What counts as a photo or video when picking or dropping files on the desktop.
#[cfg(not(target_os = "android"))]
const MEDIA_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "heic", "mp4", "mkv", "mov", "webm",
];

struct Inner {
    rt: Handle,
    data_dir: PathBuf,
    /// The UI listening now; replaced when a new one attaches (an Android activity can be recreated
    /// while the node keeps running).
    tx: Mutex<Option<mpsc::UnboundedSender<Msg>>>,
    /// What only the Activity tells (a share, the insets), kept for the UI while none listens.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    held: Mutex<Vec<Msg>>,
    core: OnceLock<Core>,
    ble_status: Mutex<String>,
    /// Counts the keystrokes in each text field (by name, id), so only the last one typed is saved
    /// (see [`Backend::after_typing`]).
    typing: Mutex<HashMap<String, u64>>,
    /// The group a system file picker was opened for (Android reports picks without one).
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    picking: Mutex<Option<String>>,
    frames: watch::Sender<Frame>,
    #[cfg(not(target_os = "android"))]
    scanner: Mutex<Option<crate::scan::Scanner>>,
    music: Music,
}

#[derive(Clone)]
pub struct Backend(Arc<Inner>);

impl PartialEq for Backend {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Backend {
    /// Start the node in `data_dir` on `rt`. Commands do nothing until it is up ([`Msg::Ready`]).
    pub fn start(rt: Handle, data_dir: PathBuf) -> Self {
        let backend = Backend(Arc::new(Inner {
            rt,
            data_dir,
            tx: Mutex::default(),
            held: Mutex::default(),
            core: OnceLock::new(),
            ble_status: Mutex::default(),
            typing: Mutex::default(),
            picking: Mutex::default(),
            frames: watch::channel(None).0,
            #[cfg(not(target_os = "android"))]
            scanner: Mutex::default(),
            music: Music::default(),
        }));
        // On Android the Bluetooth stack delivers its callbacks through the Java main thread, which
        // must stay free to run them, so nothing here waits on it.
        let this = backend.clone();
        backend.0.rt.spawn(async move { this.boot().await });
        #[cfg(target_os = "android")]
        {
            let this = backend.clone();
            flipflop::platform::set_handler(move |event| this.on_activity(event));
        }
        backend
    }

    async fn boot(self) {
        let want_ble = tokio::task::spawn_blocking(flipflop::platform::ensure_ble_permission)
            .await
            .unwrap_or(false);
        let events = self.clone();
        let core = match Core::start(&self.0.data_dir, want_ble, move |event| {
            events.send(Msg::Core(event))
        })
        .await
        {
            Ok(core) => core,
            Err(err) => return self.notice(format!("failed to start: {err:#}")),
        };
        let _ = self.0.core.set(core.clone());
        // `flipflop` hands its commands to us while the app is open.
        #[cfg(not(target_os = "android"))]
        if let Err(err) = flipflop::ipc::serve(&self.0.data_dir, core.clone()) {
            tracing::warn!("command socket: {err:#}");
        }
        core.publish_groups();
        self.send(Msg::Ready {
            core: core.clone(),
            download_dir: core.download_dir().display().to_string(),
        });

        // Two BLE statuses share one line: the iroh transport and our discovery.
        let line = Arc::new(Mutex::new((
            format!("Bluetooth transport {}", core.ble_transport_status()),
            String::new(),
        )));
        let show = {
            let (line, this) = (line.clone(), self.clone());
            move || {
                let (t, d) = line.lock().unwrap().clone();
                let text = if d.is_empty() {
                    t
                } else {
                    format!("{d} · {t}")
                };
                *this.0.ble_status.lock().unwrap() = text.clone();
                this.send(Msg::BleStatus(text));
            }
        };
        show();
        flipflop::ble::start(core, move |text| {
            line.lock().unwrap().1 = text;
            show();
        });
    }

    /// A new UI starts listening. It is told everything there is to know so far.
    pub fn attach(&self) -> mpsc::UnboundedReceiver<Msg> {
        let (tx, rx) = mpsc::unbounded_channel();
        if let Some(core) = self.core() {
            let _ = tx.send(Msg::Ready {
                core: core.clone(),
                download_dir: core.download_dir().display().to_string(),
            });
            let ble = self.0.ble_status.lock().unwrap().clone();
            let groups = core.groups();
            let mut snapshot = vec![
                Event::Name(core.name()),
                Event::Ticket(core.ticket()),
                Event::Peers(core.peers()),
                Event::Groups(groups.clone()),
                Event::Home(core.unseen()),
                Event::Transfers(core.transfers_now()),
            ];
            snapshot.extend(
                groups
                    .groups
                    .iter()
                    .filter_map(|g| core.group_view(&g.id))
                    .map(Event::Group),
            );
            for event in snapshot {
                let _ = tx.send(Msg::Core(event));
            }
            let _ = tx.send(Msg::BleStatus(ble));
        }
        let _ = tx.send(Msg::Music(self.0.music.state()));
        for msg in std::mem::take(&mut *self.0.held.lock().unwrap()) {
            let _ = tx.send(msg);
        }
        *self.0.tx.lock().unwrap() = Some(tx);
        rx
    }

    fn send(&self, msg: Msg) {
        if let Msg::Core(Event::Transfers(now)) = &msg {
            flipflop::platform::show_transfers(now);
        }
        // Listening along is the player's business; the UI learns it from its state.
        if let Msg::Core(Event::Listening {
            group,
            sharers,
            followers,
        }) = msg
        {
            return self.music().heard(self, group, sharers, followers);
        }
        let msg = match &*self.0.tx.lock().unwrap() {
            Some(tx) => match tx.send(msg) {
                Ok(()) => return,
                Err(mpsc::error::SendError(msg)) => msg,
            },
            None => msg,
        };
        // Nobody listens: keep what the next UI could not learn otherwise.
        let mut held = self.0.held.lock().unwrap();
        match msg {
            Msg::Shared { .. } => held.push(msg),
            Msg::Insets { .. } => {
                held.retain(|m| !matches!(m, Msg::Insets { .. }));
                held.push(msg);
            }
            _ => {}
        }
    }

    /// What plays: the queue and the player, which outlive the UI.
    pub fn music(&self) -> &Music {
        &self.0.music
    }

    pub(crate) fn send_music(&self, state: MusicState) {
        self.send(Msg::Music(state));
    }

    pub(crate) fn spawn_task(&self, task: impl Future<Output = ()> + Send + 'static) {
        self.0.rt.spawn(task);
    }

    pub fn notice(&self, text: impl Into<String>) {
        self.send(Msg::Notice(text.into()));
    }

    pub fn core(&self) -> Option<Core> {
        self.0.core.get().cloned()
    }

    pub fn data_dir(&self) -> &std::path::Path {
        &self.0.data_dir
    }

    /// Run `f` on the node, if it is up, and report its error.
    pub fn run(&self, f: impl FnOnce(&Core) -> anyhow::Result<()>) {
        let Some(core) = self.core() else { return };
        let _g = self.0.rt.enter();
        if let Err(err) = f(&core) {
            self.notice(format!("{err:#}"));
        }
    }

    /// Run an async job on the node in the background, telling `done` on success and the error otherwise.
    fn spawn<F, T>(&self, done: Option<&'static str>, f: impl FnOnce(Core) -> F)
    where
        F: Future<Output = anyhow::Result<T>> + Send + 'static,
    {
        let Some(core) = self.core() else { return };
        let (this, fut) = (self.clone(), f(core));
        self.0.rt.spawn(async move {
            match fut.await {
                Ok(_) => {
                    if let Some(done) = done {
                        this.notice(done)
                    }
                }
                Err(err) => this.notice(format!("{err:#}")),
            }
        });
    }

    /// Run `save` once typing in `field` pauses. Every keystroke calls this; only the last value of
    /// a burst is saved, so typing does not hit the disk (and the peers) once per character.
    fn typed(&self, field: String, save: impl FnOnce(&Core) + Send + 'static) {
        let Some(core) = self.core() else { return };
        self.after_typing(field, TYPING_PAUSE, move || save(&core));
    }

    /// Run `f` once typing in `field` pauses for `after`. Every keystroke calls this; only the last
    /// one's `f` runs, so typing does not hit the disk (and the peers) once per character.
    pub(crate) fn after_typing(
        &self,
        field: String,
        after: Duration,
        f: impl FnOnce() + Send + 'static,
    ) {
        let edit = {
            let mut typing = self.0.typing.lock().unwrap();
            let edit = typing.entry(field.clone()).or_default();
            *edit += 1;
            *edit
        };
        let inner = self.0.clone();
        self.0.rt.spawn(async move {
            tokio::time::sleep(after).await;
            // Held while `f` runs, so a save that has been replaced cannot land after the new one.
            let typing = inner.typing.lock().unwrap();
            if typing.get(&field).copied() == Some(edit) {
                f();
            }
        });
    }

    /// What was typed in `field` is settled: drop the save still waiting for it, and run `f` while
    /// holding the field, so none that lost its place can land after this one.
    pub(crate) fn settled(&self, field: &str, f: impl FnOnce()) {
        let mut typing = self.0.typing.lock().unwrap();
        *typing.entry(field.to_owned()).or_default() += 1;
        f();
    }

    /// Rename this device as its name is typed. An empty name keeps the current one (the field is
    /// being cleared, not renamed to nothing).
    pub fn set_name(&self, name: String) {
        self.typed("name".into(), move |core| {
            if !flipflop::settings::clean_name(&name).is_empty() {
                core.set_name(&name);
            }
        });
    }

    /// Give a peer a local name as it is typed. An empty name restores the name they go by.
    pub fn rename_contact(&self, id: EndpointId, name: String) {
        self.typed(format!("peer:{id}"), move |core| {
            core.rename_contact(id, &name)
        });
    }

    /// Give a group a local name as it is typed. An empty name restores the name it goes by.
    pub fn rename_group(&self, group: String, name: String) {
        self.typed(format!("group:{group}"), move |core| {
            core.rename_group(&group, &name)
        });
    }

    /// Catch the groups up, off the UI thread.
    pub fn mark_seen(&self, gids: Vec<String>) {
        if let Some(core) = self.core() {
            self.0.rt.spawn(async move {
                for gid in gids {
                    core.mark_seen(&gid);
                }
            });
        }
    }

    pub fn connect(&self, id: EndpointId) {
        self.notice("Connecting…");
        self.spawn(Some("Connected"), move |core| async move {
            core.connect(id).await
        });
    }

    pub fn connect_ticket(&self, text: String) {
        let Some(core) = self.core() else { return };
        self.notice("Connecting…");
        let this = self.clone();
        self.0.rt.spawn(async move {
            match core.connect_ticket(&text).await {
                Ok(_) => {
                    this.send(Msg::ShowPeers);
                    this.notice("Connected");
                }
                Err(err) => this.notice(format!("{err:#}")),
            }
        });
    }

    /// Offer a file to a group and report how it went.
    fn offer(&self, gid: String, path: PathBuf, name: Option<String>, kind: &str, staged: bool) {
        if kind == "music" {
            return self.spawn(Some("Offered to play"), move |core| async move {
                core.offer_music(&gid, path, name, staged).await
            });
        }
        let kind = kind.to_owned();
        self.spawn(Some("Offered to the group"), move |core| async move {
            core.offer_file(&gid, path, name, &kind, staged).await
        });
    }

    /// Read the wasm at `path`; its form opens when it is known what the app asks.
    fn prepare_app(&self, gid: String, path: PathBuf) {
        let Some(core) = self.core() else { return };
        let this = self.clone();
        self.0.rt.spawn(async move {
            match core.prepare_app(path).await {
                Ok(draft) => this.send(Msg::AppDraft { gid, draft }),
                Err(err) => this.notice(format!("{err:#}")),
            }
        });
    }

    /// Add the app to the group, named `name`, with the answers `values` to its form.
    pub fn add_app(
        &self,
        gid: String,
        draft: flipflop::core::AppDraft,
        name: String,
        values: Vec<String>,
    ) {
        self.spawn(Some("App added to the group"), move |core| async move {
            core.add_app(&gid, &draft, &name, &values).await
        });
    }

    pub fn reoffer_file(&self, gid: String, id: String) {
        self.spawn(Some("Offered to the group again"), move |core| async move {
            core.reoffer_file(&gid, &id).await
        });
    }

    pub fn download_file(&self, gid: String, id: String) {
        self.spawn(None, move |core| async move {
            core.download_file(&gid, &id).await
        });
    }

    /// Offer songs to a group's music tab, to be played.
    pub fn offer_music(&self, gid: String, paths: Vec<PathBuf>) {
        for path in paths {
            self.offer(gid.clone(), path, None, "music", false);
        }
    }

    /// Offer an installed app (one of `platform::list_apps`) to a group.
    pub fn share_app(&self, gid: String, path: String, name: String) {
        self.offer(gid, path.into(), Some(format!("{name}.apk")), "apk", false);
    }

    /// Pick files to offer to `gid` ("file" | "media" | "folder", the latter desktop only), songs to
    /// play in its music tab ("music", or "music-folder" for an album, desktop only), or an app for
    /// its tabs ("wasm").
    pub fn pick(&self, gid: String, kind: &str) {
        #[cfg(target_os = "android")]
        {
            *self.0.picking.lock().unwrap() = Some(gid);
            flipflop::platform::pick_files(kind);
        }
        #[cfg(not(target_os = "android"))]
        {
            let (this, kind) = (self.clone(), kind.to_owned());
            self.0.rt.spawn(async move {
                let dialog = rfd::AsyncFileDialog::new();
                match kind.as_str() {
                    "wasm" => {
                        let picked = dialog
                            .set_title("Add an app to the group")
                            .add_filter("WebAssembly app", &["wasm"])
                            .pick_file()
                            .await;
                        if let Some(file) = picked {
                            this.prepare_app(gid, file.path().to_owned());
                        }
                    }
                    "music" => {
                        let picked = dialog
                            .set_title("Offer songs to play in the group")
                            .add_filter("Audio", flipflop::core::AUDIO_EXTENSIONS)
                            .pick_files()
                            .await;
                        let paths = picked
                            .unwrap_or_default()
                            .iter()
                            .map(|f| f.path().to_owned())
                            .collect();
                        this.offer_music(gid, paths);
                    }
                    "music-folder" => {
                        let picked = dialog
                            .set_title("Offer a folder to play in the group, as an album")
                            .pick_folder()
                            .await;
                        if let Some(folder) = picked {
                            this.offer_music(gid, vec![folder.path().to_owned()]);
                        }
                    }
                    "folder" => {
                        let picked = dialog
                            .set_title("Offer a folder to the group")
                            .pick_folder()
                            .await;
                        if let Some(folder) = picked {
                            this.offer(gid, folder.path().to_owned(), None, "folder", false);
                        }
                    }
                    _ => {
                        let mut dialog = dialog.set_title("Offer to the group");
                        if kind == "media" {
                            dialog = dialog.add_filter("Photos and videos", MEDIA_EXTENSIONS);
                        }
                        for file in dialog.pick_files().await.unwrap_or_default() {
                            this.offer(gid.clone(), file.path().to_owned(), None, &kind, false);
                        }
                    }
                }
            });
        }
    }

    /// Offer files and folders dropped on the window to `gid`: folders as folders, photos and videos as media.
    #[cfg(not(target_os = "android"))]
    pub fn offer_paths(&self, gid: String, paths: Vec<PathBuf>) {
        for path in paths {
            let media = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| MEDIA_EXTENSIONS.contains(&e.to_lowercase().as_str()));
            let kind = if path.is_dir() {
                "folder"
            } else if media {
                "media"
            } else {
                "file"
            };
            self.offer(gid.clone(), path, None, kind, false);
        }
    }

    /// The latest webcam preview frame while scanning.
    pub fn frames(&self) -> watch::Receiver<Frame> {
        self.0.frames.subscribe()
    }

    /// Scan a code: with the webcam (desktop), or the system scanner activity (Android).
    pub fn scan(&self) {
        #[cfg(not(target_os = "android"))]
        {
            if self.core().is_none() {
                return;
            }
            self.send(Msg::Scanning(true));
            let (frames, done) = (self.clone(), self.clone());
            let scanner = crate::scan::start(
                move |w, h, rgba| {
                    frames.0.frames.send_replace(Some((w, h, rgba.into())));
                },
                move |result| {
                    done.0.frames.send_replace(None);
                    done.send(Msg::Scanning(false));
                    match result {
                        Ok(text) => done.connect_ticket(text),
                        Err(msg) if msg == "cancelled" => {}
                        Err(msg) => done.notice(msg),
                    }
                },
            );
            *self.0.scanner.lock().unwrap() = Some(scanner);
        }
        #[cfg(target_os = "android")]
        flipflop::platform::start_qr_scan();
    }

    pub fn cancel_scan(&self) {
        #[cfg(not(target_os = "android"))]
        {
            self.0.scanner.lock().unwrap().take();
            self.send(Msg::Scanning(false));
        }
    }

    /// Stop the node cleanly.
    pub fn shutdown(&self) {
        if let Some(core) = self.core() {
            self.0.rt.block_on(core.shutdown());
        }
    }

    #[cfg(target_os = "android")]
    fn on_activity(&self, event: flipflop::platform::ActivityEvent) {
        use flipflop::platform::ActivityEvent;
        match event {
            ActivityEvent::QrScanned(text) => self.connect_ticket(text),
            ActivityEvent::Shared { text, files } => self.send(Msg::Shared { text, files }),
            ActivityEvent::Insets {
                left,
                top,
                right,
                bottom,
                keyboard,
            } => self.send(Msg::Insets {
                insets: [left, top, right, bottom],
                keyboard,
            }),
            // A picked file goes to the group it was picked for, a shared one to the group chosen for it.
            ActivityEvent::FilePicked {
                kind,
                path,
                name,
                group,
            } => {
                let gid = Some(group)
                    .filter(|g| !g.is_empty())
                    .or_else(|| self.0.picking.lock().unwrap().clone());
                let Some(gid) = gid else { return };
                if kind == "wasm" {
                    return self.prepare_app(gid, path.into());
                }
                let name = Some(name).filter(|n| !n.is_empty());
                // A content:// URI is shared where it is; anything else is our own copy.
                let staged = !path.starts_with("content://");
                self.offer(gid, path.into(), name, &kind, staged);
            }
            ActivityEvent::MusicPlayer {
                ticket,
                what,
                detail,
            } => {
                let update = match what {
                    flipflop::platform::MUSIC_LOADED => Update::Loaded,
                    flipflop::platform::MUSIC_PAUSED => Update::Paused(true),
                    flipflop::platform::MUSIC_PLAYING => Update::Paused(false),
                    flipflop::platform::MUSIC_ENDED => Update::Ended,
                    _ => Update::Failed(detail),
                };
                self.music().update(self, ticket, update);
            }
            // The notification's buttons, the lock screen's, a headset's.
            ActivityEvent::MusicControl(action) => match action.as_str() {
                "toggle" => self.music().toggle(self),
                "next" => self.music().next(self),
                "stop" => self.music().clear(self),
                _ => {}
            },
            ActivityEvent::ImeText(text) => self.send(Msg::Ime(crate::model::Ime::Text(text))),
            ActivityEvent::ImeComposing(text) => {
                self.send(Msg::Ime(crate::model::Ime::Composing(text)))
            }
            ActivityEvent::ImeKey(key) => {
                use crate::model::Ime;
                let key = match key.as_str() {
                    "backspace" => Ime::Backspace,
                    "enter" => Ime::Enter,
                    "left" => Ime::Left,
                    "right" => Ime::Right,
                    "up" => Ime::Up,
                    "down" => Ime::Down,
                    "selectall" => Ime::SelectAll,
                    "copy" => Ime::Copy,
                    "cut" => Ime::Cut,
                    _ => return,
                };
                self.send(Msg::Ime(key));
            }
        }
    }
}

/// The apps that can be shared: `(path of the base APK, name, detail)`. Android only.
pub fn list_apps() -> Vec<(String, String, String)> {
    #[cfg(target_os = "android")]
    {
        flipflop::platform::list_apps()
            .into_iter()
            .map(|(path, name, detail)| {
                // The size of what would be sent: the base APK.
                let detail = match std::fs::metadata(&path) {
                    Ok(m) => format!("{} · {detail}", format::size(m.len())),
                    Err(_) => detail,
                };
                (path, name, detail)
            })
            .collect()
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = format::size;
        Vec::new()
    }
}
