//! What only the UI cares about: where we are, the open tab, dialogs, and what is waiting to be shared.

use std::path::PathBuf;

use freya::prelude::*;

use crate::backend::Backend;

#[derive(Clone, PartialEq, Debug)]
pub enum Page {
    Home,
    /// The list of groups (phones; the desktop lists them in the sidebar).
    Groups,
    /// A group's page, or a paired peer's (its implied group).
    Group(String),
    Peers,
    Add,
    Settings,
}

/// A rename / delete dialog.
#[derive(Clone, PartialEq)]
pub struct Edit {
    pub kind: EditKind,
    pub id: String,
    /// What it is called when the local name is empty.
    pub original: String,
    pub text: String,
}

#[derive(Clone, Copy, PartialEq)]
pub enum EditKind {
    Peer,
    Group,
}

/// Something to send to a group or peer the user picks: what another app shared with us (Android) or
/// was dropped on the window (desktop).
#[derive(Clone, PartialEq)]
pub enum Share {
    Shared { text: String, files: u32 },
    Dropped(Vec<PathBuf>),
}

impl Share {
    /// "a text and 2 files"
    pub fn what(&self) -> String {
        let (text, files, folders) = match self {
            Share::Shared { text, files } => (!text.is_empty(), *files as usize, 0),
            Share::Dropped(paths) => {
                let folders = paths.iter().filter(|p| p.is_dir()).count();
                (false, paths.len() - folders, folders)
            }
        };
        let count = |n: usize, one: &str, many: &str| match n {
            0 => None,
            1 => Some(one.to_owned()),
            n => Some(format!("{n} {many}")),
        };
        [
            text.then(|| "a text".to_owned()),
            count(files, "a file", "files"),
            count(folders, "a folder", "folders"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" and ")
    }

    pub fn text(&self) -> &str {
        match self {
            Share::Shared { text, .. } => text,
            Share::Dropped(_) => "",
        }
    }

    fn has_files(&self) -> bool {
        match self {
            Share::Shared { files, .. } => *files > 0,
            Share::Dropped(paths) => !paths.is_empty(),
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct Ui {
    pub page: Page,
    /// "files" | "texts" | "pad" | an app's id | "info"
    pub group_tab: String,
    /// The group whose page is showing: it counts as looked at.
    viewed: Option<String>,
    pub edit: Option<Edit>,
    /// The installed apps to share, while that picker is open (Android).
    pub apps: Option<Vec<(String, String, String)>>,
    pub share: Option<Share>,
    /// Files are being dragged over the window (desktop).
    pub drop_hover: bool,
    /// A one-line message flashing at the bottom, with a serial so the newest one is the one that clears.
    pub toast: Option<(u64, String)>,
    /// The group the peer's page invites to, as an index into `Model::invitable`.
    pub invite_choice: usize,
    /// "system" | "light" | "dark"; kept in the data dir.
    pub theme: String,
    /// Desktop sidebar width, kept in the data dir once a drag ends.
    pub sidebar_width: f32,
}

pub const SIDEBAR_MIN_WIDTH: f32 = 180.;
pub const SIDEBAR_MAX_WIDTH: f32 = 420.;

impl Ui {
    pub fn load(backend: &Backend) -> Self {
        let read = |name: &str| {
            std::fs::read_to_string(backend.data_dir().join(name))
                .map(|s| s.trim().to_owned())
                .unwrap_or_default()
        };
        let theme = read("theme");
        Self {
            page: Page::Home,
            group_tab: "files".into(),
            viewed: None,
            edit: None,
            apps: None,
            share: None,
            drop_hover: false,
            toast: None,
            invite_choice: 0,
            theme: if theme.is_empty() { "system".into() } else { theme },
            sidebar_width: read("sidebar-width")
                .parse::<f32>()
                .map_or(240., |w| w.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH)),
        }
    }

    /// The group whose page is open, if any.
    pub fn open_group(&self) -> Option<&str> {
        match &self.page {
            Page::Group(id) => Some(id),
            _ => None,
        }
    }
}

/// Handle on the UI state; `Copy`, for event handlers.
#[derive(Clone, Copy, PartialEq)]
pub struct Nav {
    pub ui: State<Ui>,
    backend: State<Backend>,
}

impl Nav {
    pub fn new(ui: State<Ui>, backend: State<Backend>) -> Self {
        Self { ui, backend }
    }

    pub fn backend(&self) -> Backend {
        self.backend.peek().clone()
    }

    pub fn touch(&self) -> bool {
        crate::theme::touch()
    }

    /// Where leaving a group goes: the groups list on phones, Home on the desktop (which has no list page).
    pub fn list_page(&self) -> Page {
        if self.touch() { Page::Groups } else { Page::Home }
    }

    pub fn go(mut self, page: Page) {
        let now = match &page {
            Page::Group(id) => Some(id.clone()),
            _ => None,
        };
        let left = {
            let mut ui = self.ui.write();
            ui.page = page;
            std::mem::replace(&mut ui.viewed, now.clone())
        };
        // The group left is caught up, so is the new one.
        if left != now {
            self.backend().mark_seen(left.into_iter().chain(now).collect());
        }
    }

    /// The group being looked at (its updates are not news).
    pub fn viewed(&self) -> Option<String> {
        self.ui.peek().viewed.clone()
    }

    /// Open a group's page, on `tab` if given, else its first tab.
    pub fn open_group(mut self, id: &str, tab: Option<&str>) {
        {
            let mut ui = self.ui.write();
            ui.group_tab = tab.unwrap_or("files").into();
            ui.invite_choice = 0;
        }
        self.go(Page::Group(id.into()));
    }

    pub fn close_group(self) {
        self.go(self.list_page());
    }

    pub fn new_group(self) {
        if let Some(core) = self.backend().core() {
            let id = core.create_group();
            self.open_group(&id, None);
        }
    }

    /// A new guest link: its group opens on the page that shows the link.
    pub fn new_guest_link(self) {
        if let Some(core) = self.backend().core() {
            let id = core.create_guest_link(false);
            self.open_group(&id, Some("info"));
        }
    }

    pub fn set_tab(mut self, tab: &str) {
        self.ui.write().group_tab = tab.into();
    }

    pub fn edit(mut self, edit: Option<Edit>) {
        self.ui.write().edit = edit;
    }

    /// Flash a one-line message for a few seconds.
    pub fn toast(mut self, text: String) {
        let serial = self.ui.peek().toast.as_ref().map_or(0, |t| t.0 + 1);
        self.ui.write().toast = Some((serial, text));
        let mut ui = self.ui;
        spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(4)).await;
            if ui.peek().toast.as_ref().is_some_and(|t| t.0 == serial) {
                ui.write().toast = None;
            }
        });
    }

    /// Send what was shared with us to this group (or paired peer's page), and show it there.
    pub fn share_to(mut self, gid: &str) {
        let Some(share) = self.ui.write().share.take() else {
            return;
        };
        self.open_group(gid, Some(if share.has_files() { "files" } else { "texts" }));
        let backend = self.backend();
        match share {
            Share::Shared { text, files } => {
                if !text.is_empty() {
                    backend.run(|core| core.send_text(gid, &text));
                }
                #[cfg(target_os = "android")]
                if files > 0 {
                    flipflop::platform::deliver_shared(gid);
                }
                let _ = files;
            }
            #[cfg(not(target_os = "android"))]
            Share::Dropped(paths) => backend.offer_paths(gid.into(), paths),
            #[cfg(target_os = "android")]
            Share::Dropped(_) => {}
        }
    }

    /// Files and folders dropped on the window: to the group whose page is showing, else ask where, as for a share.
    #[cfg(not(target_os = "android"))]
    pub fn dropped(mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let open = self.ui.peek().open_group().map(str::to_owned);
        match open {
            Some(gid) => {
                self.set_tab("files");
                self.backend().offer_paths(gid, paths);
            }
            None => self.ui.write().share = Some(Share::Dropped(paths)),
        }
    }

    /// Back (Android): dialog, then scan, then group, then Home. Returns false when nothing is left to close.
    pub fn back(mut self, scanning: bool) -> bool {
        let page = self.ui.peek().page.clone();
        let mut ui = self.ui.write();
        if ui.share.is_some() {
            ui.share = None;
        } else if ui.apps.is_some() {
            ui.apps = None;
        } else if ui.edit.is_some() {
            ui.edit = None;
        } else if scanning {
            drop(ui);
            self.backend().cancel_scan();
        } else if matches!(page, Page::Group(_)) {
            drop(ui);
            self.close_group();
        } else if page != Page::Home {
            drop(ui);
            self.go(Page::Home);
        } else {
            return false;
        }
        true
    }

    /// Offer files to the open group: "file" | "media" | "folder" | "wasm" pick from the system; "app"
    /// lists the installed apps (Android).
    pub fn pick(mut self, kind: &str) {
        if kind == "app" {
            self.ui.write().apps = Some(crate::backend::list_apps());
            return;
        }
        if let Some(gid) = self.ui.peek().open_group() {
            self.backend().pick(gid.into(), kind);
        }
    }

    /// "system" | "light" | "dark", kept in the data dir.
    pub fn set_theme(mut self, theme: &str) {
        self.ui.write().theme = theme.into();
        let _ = std::fs::write(self.backend().data_dir().join("theme"), theme);
    }

    pub fn save_sidebar_width(&self) {
        let width = self.ui.peek().sidebar_width;
        let _ = std::fs::write(
            self.backend().data_dir().join("sidebar-width"),
            width.to_string(),
        );
    }
}
