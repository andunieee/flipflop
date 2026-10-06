//! The bridge to the Kotlin Multiplatform UI (UniFFI).
//!
//! The UI is a pure view: Rust owns everything derived from [`Core`]. Whenever something changes it
//! builds a whole [`UiState`] and hands it to the [`Listener`]; the UI sends commands back by calling
//! the methods of [`App`]. What only the UI cares about (the page, the tab, what is typed in a text
//! field) stays on the Kotlin side.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
};

use anyhow::Result;
use iroh::EndpointId;
use tokio::runtime::{EnterGuard, Runtime};

use crate::{
    core::{Core, Event, GroupInfo, GroupView, GroupsView, MemberState, Peer, Unseen},
    format, groupdoc,
};

// ---- what the UI is shown ---------------------------------------------------------------------

#[derive(Clone, Default, uniffi::Record)]
pub struct PeerRow {
    pub id: String,
    /// Alias if set, else the peer's own name.
    pub title: String,
    /// The peer's own name, shown dimmed when an alias hides it.
    pub subtitle: String,
    pub alias: String,
    pub short: String,
    pub initials: String,
    pub lan: bool,
    pub ble: bool,
    pub connected: bool,
    pub saved: bool,
    /// We paired with them / they paired with us.
    pub paired: bool,
    pub paired_us: bool,
    /// Both paired: the id of the implied group (the peer's page), else "".
    pub group_id: String,
}

#[derive(Clone, Default, uniffi::Record)]
pub struct GroupRow {
    pub id: String,
    pub title: String,
    /// The haiku, shown dimmed when an alias replaces it as the title.
    pub subtitle: String,
    pub summary: String,
    pub online: bool,
    /// A guest group: anyone with its link joins from a browser.
    pub guest: bool,
}

/// What is new in one group since it was last looked at.
#[derive(Clone, Default, uniffi::Record)]
pub struct HomeRow {
    pub group_id: String,
    pub title: String,
    /// One line per kind of news, separated by newlines.
    pub lines: String,
    /// An implied group (a paired peer).
    pub paired: bool,
    /// The group tab showing the first kind of news.
    pub tab: String,
}

/// A paired peer or a group with someone online now; opens `group_id`.
#[derive(Clone, Default, uniffi::Record)]
pub struct OnlineRow {
    pub group_id: String,
    pub title: String,
}

#[derive(Clone, Default, uniffi::Record)]
pub struct InviteRow {
    pub inviter_id: String,
    pub group_id: String,
    pub inviter_name: String,
    pub group_title: String,
}

#[derive(Clone, Default, uniffi::Record)]
pub struct MemberRow {
    pub id: String,
    pub name: String,
    pub initials: String,
    pub status: String,
    /// "ok" | "active" | "warn" | "error" | "muted"
    pub tone: String,
    pub online: bool,
    pub me: bool,
}

#[derive(Clone, Default, uniffi::Record)]
pub struct CandidateRow {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Default, uniffi::Record)]
pub struct FileRow {
    pub id: String,
    pub name: String,
    pub size: String,
    /// "Alice's phone · 14:02"
    pub meta: String,
    /// 0 file, 1 photo / video, 2 app
    pub kind: i32,
    pub mine: bool,
    pub have: bool,
    pub stopped: bool,
    pub can_download: bool,
    pub downloading: bool,
    pub progress: f32,
    pub progress_label: String,
    /// "2 of 3 have it"
    pub audience: String,
    pub saved_to: String,
    pub error: String,
    /// We downloaded it and could serve it to the others too.
    pub can_host: bool,
    /// We serve it to the others.
    pub hosting: bool,
    /// "Also from Bob, Carol"
    pub hosts: String,
}

#[derive(Clone, Default, uniffi::Record)]
pub struct TextRow {
    pub id: String,
    pub name: String,
    pub text: String,
    pub time: String,
    pub mine: bool,
    pub is_link: bool,
}

#[derive(Clone, Default, uniffi::Record)]
pub struct AppRow {
    pub path: String,
    pub name: String,
    pub detail: String,
}

/// Everything the UI shows that comes from the core.
#[derive(Clone, Default, uniffi::Record)]
pub struct UiState {
    pub my_name: String,
    pub node_ready: bool,
    pub ble_status: String,
    pub download_dir: String,
    /// Our own ticket, to show as a QR code (see [`qr_matrix`]).
    pub ticket: String,
    pub scanning: bool,

    pub peers: Vec<PeerRow>,
    pub groups: Vec<GroupRow>,
    pub invites: Vec<InviteRow>,
    pub home: Vec<HomeRow>,
    pub online: Vec<OnlineRow>,

    // the open group ("" = none)
    pub open_group: String,
    pub group_title: String,
    pub group_haiku: String,
    pub group_alias: String,
    /// The open group is the implied group of a paired peer: its page is the peer's page.
    pub group_implied: bool,
    pub group_peer: Option<PeerRow>,
    /// Titles of the groups the peer could be invited to (see [`App::invite_peer_to`]).
    pub invitable: Vec<String>,
    pub members: Vec<MemberRow>,
    pub candidates: Vec<CandidateRow>,
    pub files: Vec<FileRow>,
    pub texts: Vec<TextRow>,
    pub pad_text: String,
    /// The pad text is our own last edit, which is already on screen.
    pub pad_local: bool,
    pub auto_download: bool,
    pub auto_stop: bool,
    pub auto_rehost: bool,
    /// The open group is a guest group, with these links ("" while not available) and whether guests may upload.
    pub group_guest: bool,
    pub guest_lan_url: String,
    pub guest_web_url: String,
    pub guest_uploads: bool,
}

/// A QR code: `size` x `size` modules, row by row, true = dark.
#[derive(Clone, uniffi::Record)]
pub struct QrMatrix {
    pub size: u32,
    pub dark: Vec<bool>,
}

#[uniffi::export(with_foreign)]
pub trait Listener: Send + Sync {
    /// Something changed. Called from background threads.
    fn state(&self, state: UiState);
    /// A one-line message to flash ("Copied", an error, …).
    fn notice(&self, text: String);
    /// Go to this page ("peers", …).
    fn navigate(&self, page: String);
    /// A webcam preview frame while scanning (desktop).
    fn camera_frame(&self, width: u32, height: u32, rgba: Vec<u8>);
}

#[derive(Debug, uniffi::Error)]
#[uniffi(flat_error)]
pub enum FfiError {
    Failed(String),
}

impl std::fmt::Display for FfiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let FfiError::Failed(msg) = self;
        f.write_str(msg)
    }
}

impl std::error::Error for FfiError {}

impl From<anyhow::Error> for FfiError {
    fn from(err: anyhow::Error) -> Self {
        FfiError::Failed(format!("{err:#}"))
    }
}

// ---- free functions ---------------------------------------------------------------------------

/// The phone layout (single pane, bottom navigation).
#[uniffi::export]
pub fn is_touch() -> bool {
    cfg!(target_os = "android")
}

/// Select-to-copy on Linux desktops: the selected text goes to the PRIMARY selection.
#[uniffi::export]
pub fn copy_primary(text: String) {
    crate::platform::copy_primary(&text);
}

/// The PRIMARY selection, for middle-click paste (empty where there is none).
#[uniffi::export]
pub fn paste_primary() -> String {
    crate::platform::paste_primary()
}

/// Whether scanning uses a live webcam preview in the app (the phone hands over to a scanner activity).
#[uniffi::export]
pub fn has_live_camera() -> bool {
    cfg!(not(target_os = "android"))
}

#[uniffi::export]
pub fn qr_matrix(text: String) -> Option<QrMatrix> {
    use qrcode::{Color, QrCode};
    let code = QrCode::new(text.as_bytes()).ok()?;
    Some(QrMatrix { size: code.width() as u32, dark: code.to_colors().into_iter().map(|c| c == Color::Dark).collect() })
}

/// Where a cursor at `cursor` (a character index into `old`) ends up in `new`.
#[uniffi::export]
pub fn map_cursor(old: String, new: String, cursor: u32) -> u32 {
    groupdoc::map_cursor(&old, &new, cursor as usize) as u32
}

/// The apps that can be shared (Android only).
#[uniffi::export]
pub fn list_apps() -> Vec<AppRow> {
    #[cfg(target_os = "android")]
    {
        crate::platform::list_apps()
            .into_iter()
            .map(|(path, name, detail)| {
                // The size of what would be sent: the base APK.
                let detail = match std::fs::metadata(&path) {
                    Ok(m) => format!("{} · {detail}", format::size(m.len())),
                    Err(_) => detail,
                };
                AppRow { path, name, detail }
            })
            .collect()
    }
    #[cfg(not(target_os = "android"))]
    {
        Vec::new()
    }
}

// ---- the app ----------------------------------------------------------------------------------

/// What we remember between events.
#[derive(Default)]
struct Shared {
    ui: UiState,
    peers: Vec<Peer>,
    groups: GroupsView,
    views: HashMap<String, GroupView>,
    open_group: Option<String>,
    /// What is new in each group (the Home page).
    home: Vec<Unseen>,
    /// The page the UI shows. A group counts as looked at while its page is the one.
    page: String,
    /// The group whose page is showing.
    viewed: Option<String>,
    /// The pad text is our own last edit.
    pad_local: bool,
}

struct Inner {
    rt: Runtime,
    listener: Arc<dyn Listener>,
    shared: Mutex<Shared>,
    /// Filled once the background start-up has produced a [`Core`]; commands do nothing before that.
    core: OnceLock<Core>,
    #[cfg(not(target_os = "android"))]
    scanner: Mutex<Option<crate::scan::Scanner>>,
}

#[derive(uniffi::Object)]
pub struct App {
    inner: Arc<Inner>,
}

impl Inner {
    /// Core methods may spawn tasks, so they run inside the runtime.
    fn enter(&self) -> EnterGuard<'_> {
        self.rt.enter()
    }

    fn core(&self) -> Option<Core> {
        self.core.get().cloned()
    }

    fn notice(&self, text: impl Into<String>) {
        self.listener.notice(text.into());
    }

    /// Tell the UI how things are now.
    fn emit(&self) {
        let state = build_state(&self.shared.lock().unwrap());
        self.listener.state(state);
    }

    fn change(&self, f: impl FnOnce(&mut Shared)) {
        f(&mut self.shared.lock().unwrap());
        self.emit();
    }

    fn open_group_id(&self) -> Option<String> {
        self.shared.lock().unwrap().open_group.clone()
    }

    /// The group the user is looking at changed (`None`: none): the one they left is caught up, so is
    /// the new one.
    fn looking_at(&self, now: Option<String>) {
        let left = std::mem::replace(&mut self.shared.lock().unwrap().viewed, now.clone());
        self.mark_seen(left.into_iter().chain(now).collect());
    }

    /// Catch the groups up. Never inline: events arrive while core holds its publish lock, and
    /// marking a group seen publishes again.
    fn mark_seen(&self, gids: Vec<String>) {
        if let Some(core) = self.core() {
            self.rt.spawn(async move {
                for gid in gids {
                    core.mark_seen(&gid);
                }
            });
        }
    }

    /// Offer a file to a group from a background task and report how it went.
    #[allow(clippy::too_many_arguments)]
    fn offer(self: &Arc<Self>, core: &Core, gid: String, path: PathBuf, name: Option<String>, kind: &str, staged: bool) {
        let (this, core, kind) = (self.clone(), core.clone(), kind.to_owned());
        self.rt.spawn(async move {
            match core.offer_file(&gid, path, name, &kind, staged).await {
                Ok(_) => this.notice("Offered to the group"),
                Err(err) => this.notice(format!("{err:#}")),
            }
        });
    }

    fn connect_ticket(self: &Arc<Self>, core: &Core, text: String) {
        let (this, core) = (self.clone(), core.clone());
        self.notice("Connecting…");
        self.rt.spawn(async move {
            match core.connect_ticket(&text).await {
                Ok(_) => {
                    this.listener.navigate("peers".into());
                    this.notice("Connected");
                }
                Err(err) => this.notice(format!("{err:#}")),
            }
        });
    }

    fn on_event(self: &Arc<Self>, event: Event) {
        match event {
            Event::Name(name) => self.change(|s| s.ui.my_name = name),
            Event::Notice(text) => self.notice(text),
            Event::Ticket(ticket) => self.change(|s| s.ui.ticket = ticket),
            Event::Peers(peers) => self.change(|s| s.peers = peers),
            Event::Groups(view) => {
                let gone = {
                    let mut shared = self.shared.lock().unwrap();
                    shared.groups = view;
                    let ids: Vec<String> = shared.groups.groups.iter().map(|g| g.id.clone()).collect();
                    shared.views.retain(|id, _| ids.contains(id));
                    let gone = shared.open_group.as_ref().is_some_and(|id| !ids.contains(id));
                    if gone {
                        shared.open_group = None;
                    }
                    gone
                };
                self.emit();
                if gone {
                    self.looking_at(None);
                }
            }
            Event::Group(view) => {
                let gid = view.id.clone();
                let looking = {
                    let mut shared = self.shared.lock().unwrap();
                    let opened = shared.open_group.as_deref() == Some(view.id.as_str());
                    // Our own scratchpad edits are not news; they are caught up when the page is left.
                    let looking = opened && shared.viewed.as_deref() == Some(view.id.as_str()) && !view.scratch_local;
                    shared.views.insert(view.id.clone(), view);
                    looking
                };
                self.emit();
                if looking {
                    self.mark_seen(vec![gid]);
                }
            }
            Event::Home(news) => self.change(|s| s.home = news),
            Event::Message { .. } => {}
        }
    }
}

#[uniffi::export]
impl App {
    /// Start the node in `data_dir`. Commands do nothing until it is up (`UiState::node_ready`).
    #[uniffi::constructor]
    pub fn new(data_dir: String, listener: Arc<dyn Listener>) -> Result<Arc<Self>, FfiError> {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().map_err(anyhow::Error::from)?;
        let inner = Arc::new(Inner {
            rt,
            listener,
            shared: Mutex::default(),
            core: OnceLock::new(),
            #[cfg(not(target_os = "android"))]
            scanner: Mutex::default(),
        });
        let data_dir = PathBuf::from(data_dir);

        // Core starts in the background: on Android the Bluetooth stack delivers its callbacks through
        // the Java main thread, which must stay free to run them.
        {
            let inner = inner.clone();
            let data_dir = data_dir.clone();
            inner.clone().rt.spawn(async move {
                let want_ble = tokio::task::spawn_blocking(crate::platform::ensure_ble_permission).await.unwrap_or(false);
                let events = inner.clone();
                let core = match Core::start(&data_dir, want_ble, move |event| events.on_event(event)).await {
                    Ok(core) => core,
                    Err(err) => return inner.notice(format!("failed to start: {err:#}")),
                };
                let _ = inner.core.set(core.clone());
                // `flipflop-cli` hands its commands to us while the app is open.
                #[cfg(not(target_os = "android"))]
                if let Err(err) = crate::ipc::serve(&data_dir, core.clone()) {
                    tracing::warn!("command socket: {err:#}");
                }
                core.publish_groups();
                let dir = core.download_dir().display().to_string();
                inner.change(|s| {
                    s.ui.node_ready = true;
                    s.ui.download_dir = dir;
                });

                // Two BLE statuses share one line: the iroh transport and our discovery.
                let line = Arc::new(Mutex::new((format!("Bluetooth transport {}", core.ble_transport_status()), String::new())));
                let show = {
                    let (line, inner) = (line.clone(), inner.clone());
                    move || {
                        let (t, d) = line.lock().unwrap().clone();
                        let text = if d.is_empty() { t } else { format!("{d} · {t}") };
                        inner.change(|s| s.ui.ble_status = text);
                    }
                };
                show();
                crate::ble::start(core, move |text| {
                    line.lock().unwrap().1 = text;
                    show();
                });
            });
        }

        #[cfg(target_os = "android")]
        {
            let qr = inner.clone();
            crate::platform::set_qr_handler(move |text| {
                if let Some(core) = qr.core() {
                    qr.connect_ticket(&core, text);
                }
            });
            // A picked file is offered to whichever group is open.
            let files = inner.clone();
            crate::platform::set_file_handler(move |kind, path, name| {
                let (Some(core), Some(gid)) = (files.core(), files.open_group_id()) else { return };
                // A content:// URI is shared where it is; anything else is our own copy.
                let staged = !path.starts_with("content://");
                files.offer(&core, gid, PathBuf::from(path), Some(name).filter(|n| !n.is_empty()), &kind, staged);
            });
        }

        Ok(Arc::new(App { inner }))
    }

    /// Stop the node cleanly.
    pub fn shutdown(&self) {
        if let Some(core) = self.inner.core() {
            self.inner.rt.block_on(core.shutdown());
        }
    }

    /// The latest state (the listener also gets it on every change).
    pub fn state(&self) -> UiState {
        build_state(&self.inner.shared.lock().unwrap())
    }

    // ---- peers and groups ----

    pub fn rename(&self, name: String) {
        let _g = self.inner.enter();
        if let Some(core) = self.inner.core() {
            core.set_name(&name);
        }
    }

    pub fn connect(&self, id: String) {
        let _g = self.inner.enter();
        let (Some(core), Ok(id)) = (self.inner.core(), id.parse::<EndpointId>()) else { return };
        let inner = self.inner.clone();
        inner.notice("Connecting…");
        self.inner.rt.spawn(async move {
            match core.connect(id).await {
                Ok(()) => inner.notice("Connected"),
                Err(err) => inner.notice(format!("{err:#}")),
            }
        });
    }

    pub fn connect_ticket(&self, text: String) {
        let _g = self.inner.enter();
        if let Some(core) = self.inner.core() {
            self.inner.connect_ticket(&core, text);
        }
    }

    /// Create a group and open it; returns its id.
    pub fn new_group(&self) -> Option<String> {
        let _g = self.inner.enter();
        let id = self.inner.core()?.create_group();
        self.open_group(Some(id.clone()));
        Some(id)
    }

    /// Create a guest group and open it; returns its id.
    pub fn new_guest_link(&self, uploads: bool) -> Option<String> {
        let _g = self.inner.enter();
        let id = self.inner.core()?.create_guest_link(uploads);
        self.open_group(Some(id.clone()));
        Some(id)
    }

    /// Turn the open group into a guest group and generate its link.
    pub fn enable_guest_link(&self, uploads: bool) {
        let _g = self.inner.enter();
        if let (Some(core), Some(gid)) = (self.inner.core(), self.inner.open_group_id()) {
            core.enable_guest_link(&gid, uploads);
        }
    }

    /// Turn the open guest group back into a plain group.
    pub fn disable_guest_link(&self) {
        let _g = self.inner.enter();
        if let (Some(core), Some(gid)) = (self.inner.core(), self.inner.open_group_id()) {
            core.disable_guest_link(&gid);
        }
    }

    /// Allow or forbid uploads on the open guest group's link.
    pub fn set_guest_uploads(&self, on: bool) {
        let _g = self.inner.enter();
        if let (Some(core), Some(gid)) = (self.inner.core(), self.inner.open_group_id()) {
            core.set_guest_uploads(&gid, on);
        }
    }

    /// Open a group's page (`None`: leave it). The UI decides which page that is.
    pub fn open_group(&self, id: Option<String>) {
        {
            let mut shared = self.inner.shared.lock().unwrap();
            shared.open_group = id.clone();
            shared.pad_local = false;
        }
        self.inner.emit();
        // Going from one group straight to another does not change the page.
        self.inner.looking_at(id);
    }

    /// The UI changed page (so we know when a group is being looked at).
    pub fn page_changed(&self, page: String) {
        let open = {
            let mut shared = self.inner.shared.lock().unwrap();
            shared.page = page.clone();
            shared.open_group.clone().filter(|_| page == "group")
        };
        self.inner.looking_at(open);
    }

    pub fn add_to_group(&self, group: String, peer: String) {
        let _g = self.inner.enter();
        if let (Some(core), Ok(peer)) = (self.inner.core(), peer.parse::<EndpointId>()) {
            core.add_to_group(&group, peer);
        }
    }

    pub fn pair_peer(&self, id: String) {
        let _g = self.inner.enter();
        if let (Some(core), Ok(id)) = (self.inner.core(), id.parse::<EndpointId>()) {
            core.pair(id);
        }
    }

    pub fn unpair_peer(&self, id: String) {
        let _g = self.inner.enter();
        if let (Some(core), Ok(id)) = (self.inner.core(), id.parse::<EndpointId>()) {
            core.unpair(id);
        }
    }

    /// Invite the open peer to the group at this index of `UiState::invitable`.
    pub fn invite_peer_to(&self, index: i32) {
        let _g = self.inner.enter();
        let Some(core) = self.inner.core() else { return };
        let picked = {
            let shared = self.inner.shared.lock().unwrap();
            let peer = shared.open_group.as_ref().and_then(|id| shared.groups.groups.iter().find(|g| g.id == *id)).and_then(|g| g.with);
            let group = usize::try_from(index).ok().and_then(|i| invitable_groups(&shared).get(i).map(|g| (g.id.clone(), group_title(g))));
            peer.zip(group)
        };
        let Some((peer, (group, title))) = picked else { return };
        core.add_to_group(&group, peer);
        self.inner.notice(format!("Invited to {title}"));
    }

    pub fn mark_all_seen(&self) {
        if let Some(core) = self.inner.core() {
            core.mark_all_seen();
        }
    }

    pub fn accept_invite(&self, inviter: String, group: String) {
        let _g = self.inner.enter();
        if let (Some(core), Ok(inviter)) = (self.inner.core(), inviter.parse::<EndpointId>()) {
            core.accept_invite(inviter, &group);
        }
    }

    pub fn reject_invite(&self, inviter: String, group: String) {
        let _g = self.inner.enter();
        if let (Some(core), Ok(inviter)) = (self.inner.core(), inviter.parse::<EndpointId>()) {
            core.reject_invite(inviter, &group);
        }
    }

    /// Rename a peer ("peer") or a group ("group"); an empty text removes the name.
    pub fn save_edit(&self, kind: String, id: String, text: String) {
        let _g = self.inner.enter();
        let Some(core) = self.inner.core() else { return };
        match kind.as_str() {
            "peer" => {
                if let Ok(id) = id.parse::<EndpointId>() {
                    core.rename_contact(id, &text);
                }
            }
            "group" => core.rename_group(&id, &text),
            _ => {}
        }
    }

    /// Forget a peer ("peer") or delete a group ("group").
    pub fn delete_edit(&self, kind: String, id: String) {
        let _g = self.inner.enter();
        let Some(core) = self.inner.core() else { return };
        match kind.as_str() {
            "peer" => {
                if let Ok(id) = id.parse::<EndpointId>() {
                    core.forget_contact(id);
                    core.disconnect(id);
                }
            }
            "group" => {
                self.open_group(None);
                core.delete_group(&id);
            }
            _ => {}
        }
    }

    // ---- group content ----

    pub fn send_text(&self, text: String) {
        let _g = self.inner.enter();
        let (Some(core), Some(gid)) = (self.inner.core(), self.inner.open_group_id()) else { return };
        if let Err(err) = core.send_text(&gid, &text) {
            self.inner.notice(format!("{err:#}"));
        }
    }

    pub fn delete_text(&self, id: String) {
        let _g = self.inner.enter();
        let (Some(core), Some(gid)) = (self.inner.core(), self.inner.open_group_id()) else { return };
        if let Err(err) = core.delete_text(&gid, &id) {
            self.inner.notice(format!("{err:#}"));
        }
    }

    pub fn copy_text(&self, text: String) {
        crate::platform::copy_text(&text);
        self.inner.notice("Copied");
    }

    pub fn paste_text(&self) -> String {
        crate::platform::paste_text()
    }

    pub fn open_link(&self, url: String) {
        crate::platform::open_url(url.trim());
    }

    /// Open a downloaded file (by its path) with the system's default app.
    pub fn open_file(&self, path: String) {
        crate::platform::open_file(&path);
    }

    /// The scratchpad's text was edited here.
    pub fn pad_edited(&self, text: String) {
        let _g = self.inner.enter();
        if let (Some(core), Some(gid)) = (self.inner.core(), self.inner.open_group_id()) {
            self.inner.shared.lock().unwrap().pad_local = true;
            core.set_scratch(&gid, &text);
        }
    }

    /// Pick files to offer to the open group ("file" | "media"), or list the apps to share ("app", see
    /// [`list_apps`]; the UI shows the picker).
    pub fn pick(&self, kind: String) {
        #[cfg(target_os = "android")]
        {
            crate::platform::pick_files(&kind);
        }
        #[cfg(not(target_os = "android"))]
        {
            let _g = self.inner.enter();
            let (Some(core), Some(gid)) = (self.inner.core(), self.inner.open_group_id()) else { return };
            let inner = self.inner.clone();
            let media = kind == "media";
            self.inner.rt.spawn(async move {
                let mut dialog = rfd::AsyncFileDialog::new().set_title("Offer to the group");
                if media {
                    dialog = dialog.add_filter("Photos and videos", &["jpg", "jpeg", "png", "gif", "webp", "heic", "mp4", "mkv", "mov", "webm"]);
                }
                for file in dialog.pick_files().await.unwrap_or_default() {
                    let kind = if media { "media" } else { "file" };
                    inner.offer(&core, gid.clone(), file.path().to_owned(), None, kind, false);
                }
            });
        }
    }

    /// Offer an installed app (one of `list_apps`) to the open group.
    pub fn share_app(&self, path: String, name: String) {
        let _g = self.inner.enter();
        let (Some(core), Some(gid)) = (self.inner.core(), self.inner.open_group_id()) else { return };
        self.inner.offer(&core, gid, PathBuf::from(path), Some(format!("{name}.apk")), "apk", false);
    }

    pub fn stop_file(&self, id: String) {
        let _g = self.inner.enter();
        let (Some(core), Some(gid)) = (self.inner.core(), self.inner.open_group_id()) else { return };
        if let Err(err) = core.stop_file(&gid, &id) {
            self.inner.notice(format!("{err:#}"));
        }
    }

    /// Serve a downloaded file to the group too (`on`), or stop.
    pub fn host_file(&self, id: String, on: bool) {
        let _g = self.inner.enter();
        let (Some(core), Some(gid)) = (self.inner.core(), self.inner.open_group_id()) else { return };
        let result = if on { core.host_file(&gid, &id) } else { core.unhost_file(&gid, &id) };
        if let Err(err) = result {
            self.inner.notice(format!("{err:#}"));
        }
    }

    pub fn download_file(&self, id: String) {
        let _g = self.inner.enter();
        let (Some(core), Some(gid)) = (self.inner.core(), self.inner.open_group_id()) else { return };
        let inner = self.inner.clone();
        self.inner.rt.spawn(async move {
            if let Err(err) = core.download_file(&gid, &id).await {
                inner.notice(format!("{err:#}"));
            }
        });
    }

    pub fn set_auto_download(&self, on: bool) {
        if let (Some(core), Some(gid)) = (self.inner.core(), self.inner.open_group_id()) {
            core.set_group_settings(&gid, Some(on), None, None);
        }
    }

    pub fn set_auto_stop(&self, on: bool) {
        if let (Some(core), Some(gid)) = (self.inner.core(), self.inner.open_group_id()) {
            core.set_group_settings(&gid, None, Some(on), None);
        }
    }

    pub fn set_auto_rehost(&self, on: bool) {
        if let (Some(core), Some(gid)) = (self.inner.core(), self.inner.open_group_id()) {
            core.set_group_settings(&gid, None, None, Some(on));
        }
    }

    // ---- QR scanning: webcam on desktop, system scanner activity on Android ----

    pub fn scan(&self) {
        #[cfg(not(target_os = "android"))]
        {
            let _g = self.inner.enter();
            let Some(core) = self.inner.core() else { return };
            self.inner.change(|s| s.ui.scanning = true);
            let (frames, done) = (self.inner.clone(), self.inner.clone());
            let scanner = crate::scan::start(
                move |w, h, rgba| frames.listener.camera_frame(w, h, rgba),
                move |result| {
                    done.change(|s| s.ui.scanning = false);
                    match result {
                        Ok(text) => done.connect_ticket(&core, text),
                        Err(msg) if msg == "cancelled" => {}
                        Err(msg) => done.notice(msg),
                    }
                },
            );
            *self.inner.scanner.lock().unwrap() = Some(scanner);
        }
        #[cfg(target_os = "android")]
        crate::platform::start_qr_scan();
    }

    pub fn cancel_scan(&self) {
        #[cfg(not(target_os = "android"))]
        {
            self.inner.scanner.lock().unwrap().take();
            self.inner.change(|s| s.ui.scanning = false);
        }
    }
}

// ---- building the state -----------------------------------------------------------------------

fn peer_row(p: &Peer) -> PeerRow {
    PeerRow {
        id: p.id.to_string(),
        title: p.alias.clone().unwrap_or_else(|| p.name.clone()),
        subtitle: if p.alias.is_some() { p.name.clone() } else { String::new() },
        alias: p.alias.clone().unwrap_or_default(),
        short: p.id.fmt_short().to_string(),
        initials: format::initials(p.alias.as_ref().unwrap_or(&p.name)),
        lan: p.lan,
        ble: p.ble,
        connected: p.connected,
        saved: p.saved,
        paired: p.paired,
        paired_us: p.paired_us,
        group_id: p.pair_group.clone().unwrap_or_default(),
    }
}

/// An implied group is named after the other device.
fn group_title(g: &GroupInfo) -> String {
    match g.with.and_then(|w| g.members.iter().find(|m| m.id == w)) {
        Some(m) => m.name.clone(),
        None => g.alias.clone().unwrap_or_else(|| g.haiku.clone()),
    }
}

/// The groups the open peer page offers to invite the peer to, in the order of the dropdown: the
/// listed groups the peer is not part of (a declined invite can be sent again).
fn invitable_groups(shared: &Shared) -> Vec<&GroupInfo> {
    let Some(peer) = shared.open_group.as_ref().and_then(|id| shared.groups.groups.iter().find(|g| g.id == *id)).and_then(|g| g.with) else {
        return Vec::new();
    };
    let key = peer.to_string();
    shared
        .groups
        .groups
        .iter()
        .filter(|g| g.with.is_none())
        .filter(|g| !g.members.iter().any(|m| m.id == peer && m.state != MemberState::Declined))
        .filter(|g| !shared.views.get(&g.id).is_some_and(|v| v.members.iter().any(|m| m.id == key)))
        .collect()
}

fn build_state(shared: &Shared) -> UiState {
    let mut ui = shared.ui.clone();
    ui.peers = shared.peers.iter().map(peer_row).collect();

    ui.groups = shared
        .groups
        .groups
        .iter()
        .filter(|g| g.with.is_none())
        .map(|g| {
            let view = shared.views.get(&g.id);
            let joined = view.map_or(1, |v| v.members.len().max(1));
            let others_online = view.is_some_and(|v| v.members.iter().any(|m| m.online && !m.me));
            let waiting = g.members.iter().filter(|m| matches!(m.state, MemberState::Pending | MemberState::Sent)).count();
            let mut summary = format!("{joined} member{}", if joined == 1 { "" } else { "s" });
            if waiting > 0 {
                summary += &format!(" · {waiting} invited");
            }
            if let Some(v) = view {
                let files = v.files.iter().filter(|f| !f.stopped).count();
                if files > 0 {
                    summary += &format!(" · {files} file{} on offer", if files == 1 { "" } else { "s" });
                }
            }
            GroupRow {
                id: g.id.clone(),
                title: group_title(g),
                subtitle: if g.alias.is_some() { g.haiku.clone() } else { String::new() },
                summary,
                online: others_online,
                guest: g.guest.is_some(),
            }
        })
        .collect();
    ui.online = ui
        .peers
        .iter()
        .filter(|p| p.connected && !p.group_id.is_empty())
        .map(|p| OnlineRow { group_id: p.group_id.clone(), title: p.title.clone() })
        .chain(ui.groups.iter().filter(|g| g.online).map(|g| OnlineRow { group_id: g.id.clone(), title: g.title.clone() }))
        .collect();
    ui.invites = shared
        .groups
        .invites
        .iter()
        .map(|i| InviteRow {
            inviter_id: i.inviter.to_string(),
            group_id: i.group.clone(),
            inviter_name: i.inviter_name.clone(),
            group_title: i.haiku.clone(),
        })
        .collect();
    ui.home = shared
        .home
        .iter()
        .map(|n| HomeRow {
            group_id: n.group.clone(),
            title: n.title.clone(),
            lines: n.lines().join("\n"),
            paired: n.with.is_some(),
            tab: n.tab().into(),
        })
        .collect();

    ui.open_group = shared.open_group.clone().unwrap_or_default();
    let Some(info) = shared.open_group.as_ref().and_then(|id| shared.groups.groups.iter().find(|g| g.id == *id)) else {
        return ui;
    };
    ui.group_title = group_title(info);
    ui.group_haiku = info.haiku.clone();
    ui.group_alias = info.alias.clone().unwrap_or_default();
    ui.group_implied = info.with.is_some();
    ui.group_peer = info.with.and_then(|w| shared.peers.iter().find(|p| p.id == w)).map(peer_row);
    ui.auto_download = info.auto_download;
    ui.auto_stop = info.auto_stop;
    ui.auto_rehost = info.auto_rehost;
    ui.group_guest = info.guest.is_some();
    ui.guest_lan_url = info.guest.as_ref().and_then(|g| g.lan_url.clone()).unwrap_or_default();
    ui.guest_web_url = info.guest.as_ref().and_then(|g| g.web_url.clone()).unwrap_or_default();
    ui.guest_uploads = info.guest.as_ref().is_some_and(|g| g.uploads);
    let view = shared.views.get(&info.id);

    // Members: who the document lists, then invitees that have not joined.
    let mut members: Vec<MemberRow> = view
        .map(|v| {
            v.members
                .iter()
                .map(|m| MemberRow {
                    id: m.id.clone(),
                    name: m.name.clone(),
                    initials: format::initials(&m.name),
                    status: if m.me { "this device" } else if m.online { "online" } else { "offline" }.into(),
                    tone: if m.me || m.online { "ok" } else { "muted" }.into(),
                    online: m.online,
                    me: m.me,
                })
                .collect()
        })
        .unwrap_or_default();
    for m in &info.members {
        if members.iter().any(|row| row.id == m.id.to_string()) {
            continue;
        }
        let (status, tone) = match m.state {
            MemberState::Accepted => ("joined", "ok"),
            MemberState::Sent => ("invited", "active"),
            MemberState::Pending => ("waiting for them to be online", "warn"),
            MemberState::Declined => ("declined", "error"),
        };
        members.push(MemberRow {
            id: m.id.to_string(),
            name: m.name.clone(),
            initials: format::initials(&m.name),
            status: status.into(),
            tone: tone.into(),
            online: false,
            me: false,
        });
    }
    ui.invitable = invitable_groups(shared).iter().map(|g| group_title(g)).collect();
    // Members the document lists (e.g. everyone but the inviter, after accepting) are not candidates.
    ui.candidates = info
        .candidates
        .iter()
        .filter(|(id, _)| !members.iter().any(|row| row.id == id.to_string()))
        .map(|(id, name)| CandidateRow { id: id.to_string(), name: name.clone() })
        .collect();
    ui.members = members;

    let Some(view) = view else { return ui };
    ui.files = view
        .files
        .iter()
        .map(|f| {
            let downloading = f.progress.is_some();
            let (done, total) = f.progress.unwrap_or((0, f.size));
            FileRow {
                id: f.id.clone(),
                name: f.name.clone(),
                size: format::size(f.size),
                meta: format!("{} · {}", if f.mine { "you".to_string() } else { f.from_name.clone() }, format::time(f.ts)),
                kind: match f.kind.as_str() {
                    "media" => 1,
                    "apk" => 2,
                    _ => 0,
                },
                mine: f.mine,
                have: f.have,
                stopped: f.stopped,
                can_download: !f.mine && !f.have && !f.stopped && !downloading && f.source_online,
                downloading,
                progress: if total == 0 { 0.0 } else { done as f32 / total as f32 },
                progress_label: format!("{} of {}", format::size(done), format::size(total)),
                audience: if f.audience == 0 { String::new() } else { format!("{} of {} have it", f.downloaded_by.len(), f.audience) },
                saved_to: f.saved_to.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
                error: f.error.clone().unwrap_or_default(),
                can_host: !f.mine && f.have && !f.stopped && !f.hosting && f.saved_to.is_some(),
                hosting: f.hosting && !f.stopped,
                hosts: if f.hosts.is_empty() || f.stopped { String::new() } else { format!("Also from {}", f.hosts.join(", ")) },
            }
        })
        .collect();
    ui.texts = view
        .texts
        .iter()
        .map(|t| TextRow {
            id: t.id.clone(),
            name: t.name.clone(),
            text: t.text.clone(),
            time: format::time(t.ts),
            mine: t.mine,
            is_link: format::is_link(&t.text),
        })
        .collect();
    ui.pad_text = view.scratch.clone();
    ui.pad_local = view.scratch_local;
    ui
}
