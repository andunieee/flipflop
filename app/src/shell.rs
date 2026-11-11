//! The frame around the pages: the sidebar (desktop) or app bar and bottom navigation (phone), the
//! messages that flash, and the dialogs.

use freya::{prelude::*, radio::use_radio};

use crate::{
    group::GroupPage,
    icons::{Icon, LOGO},
    model::{Channel, Model, group_title, peer_title},
    nav::{EditKind, Nav, Page, SIDEBAR_MAX_WIDTH, SIDEBAR_MIN_WIDTH},
    pages::{AddPage, GroupsPage, HomePage, PeersPage, ScanView, SettingsPage},
    theme::{Dim, look},
    widgets::*,
};

#[derive(PartialEq)]
pub struct Shell;

impl Component for Shell {
    fn render(&self) -> impl IntoElement {
        let (c, d) = look();
        let nav = use_consume::<Nav>();
        let me = use_radio::<Model, Channel>(Channel::Me);
        let (scanning, insets) = {
            let m = me.read();
            (m.scanning, m.insets)
        };
        let ui = nav.ui.read();
        let page = ui.page.clone();
        let touch = d.touch;

        let on_back = move |e: Event<KeyboardEventData>| {
            if e.key == Key::Named(NamedKey::BrowserBack) && !nav.back(scanning) {
                #[cfg(target_os = "android")]
                flipflop::platform::move_to_back();
            }
        };

        let content = rect()
            .width(Size::flex(1.))
            .height(Size::fill())
            .child(match page {
                _ if scanning && !touch => ScanView.into_element(),
                Page::Home => HomePage.into_element(),
                Page::Groups => GroupsPage.into_element(),
                Page::Group(id) => GroupPage { id }.into_element(),
                Page::Peers => PeersPage.into_element(),
                Page::Add => AddPage.into_element(),
                Page::Settings => SettingsPage.into_element(),
            });

        rect()
            .expanded()
            .background(c.window_bg)
            .padding((insets[1], insets[2], insets[3], insets[0]))
            .content(Content::flex())
            .on_global_key_down(on_back)
            .map(cfg!(not(target_os = "android")).then_some(()), |el, ()| {
                el.on_global_file_hover(move |_| {
                    let mut ui = nav.ui;
                    ui.write().drop_hover = true;
                })
                .on_global_file_hover_cancelled(move |_| {
                    let mut ui = nav.ui;
                    ui.write().drop_hover = false;
                })
                .on_file_drop(move |e: Event<FileEventData>| {
                    let mut ui = nav.ui;
                    ui.write().drop_hover = false;
                    #[cfg(not(target_os = "android"))]
                    nav.dropped(e.file_paths.clone());
                    let _ = e;
                })
            })
            .maybe(!touch, |el| el.child(ContextMenuViewer::new()))
            .maybe(touch, |el| el.child(AppBar))
            .child(
                rect()
                    .horizontal()
                    .width(Size::fill())
                    .height(Size::flex(1.))
                    .content(Content::flex())
                    .maybe(!touch, |el| el.child(Sidebar))
                    .child(content),
            )
            .maybe(touch, |el| el.child(BottomNav))
            .map(ui.toast.clone(), |el, (_, text)| {
                el.child(
                    rect()
                        .layer(Layer::Overlay)
                        .position(Position::new_global().bottom(if touch { 80. } else { 24. } + insets[3]))
                        .width(Size::window_percent(100.))
                        .cross_align(Alignment::Center)
                        .child(
                            rect()
                                .margin((0., 16.))
                                .corner_radius(Dim::RADIUS)
                                .background(c.text_strong)
                                .shadow(Shadow::new().blur(12.).color(Color::BLACK.with_a(60)))
                                .padding((10., 16.))
                                .child(
                                    txt(text, d.font_sm(), c.window_bg, 400),
                                ),
                        ),
                )
            })
            .maybe(ui.edit.is_some(), |el| el.child(EditDialog))
            .maybe(ui.apps.is_some(), |el| el.child(AppPicker))
            .maybe(ui.share.is_some(), |el| el.child(SharePicker))
            .maybe(ui.drop_hover, |el| el.child(DropHint))
    }
}

/// The logo, name and node status.
fn brand(logo: f32, status: impl IntoElement) -> Rect {
    let (c, d) = look();
    rect()
        .horizontal()
        .spacing(Dim::SPACE3)
        .cross_align(Alignment::Center)
        .child(
            ImageViewer::new(("logo", LOGO))
                .width(Size::px(logo))
                .height(Size::px(logo)),
        )
        .child(
            rect()
                .spacing(1.)
                .child(txt("FlipFlop", d.font_lg(), c.text_strong, 700))
                .child(status),
        )
}

#[derive(PartialEq)]
struct NodeStatus;

impl Component for NodeStatus {
    fn render(&self) -> impl IntoElement {
        let (c, d) = look();
        let me = use_radio::<Model, Channel>(Channel::Me);
        let m = me.read();
        let ready = m.core.is_some();
        rect()
            .horizontal()
            .spacing(6.)
            .cross_align(Alignment::Center)
            .child(dot(ready, 7.))
            .child(
                txt(
                    match () {
                        _ if !ready => "Starting…".to_owned(),
                        _ if m.name.is_empty() => "Online".to_owned(),
                        _ => m.name.clone(),
                    },
                    d.font_xs(),
                    if ready { c.text_muted } else { c.warn },
                    400,
                )
                .elide(),
            )
    }
}

/// Phone app bar.
#[derive(PartialEq)]
struct AppBar;

impl Component for AppBar {
    fn render(&self) -> impl IntoElement {
        let (c, d) = look();
        rect()
            .width(Size::fill())
            .background(c.window_bg)
            .child(
                rect()
                    .horizontal()
                    .width(Size::fill())
                    .height(Size::px(56.))
                    .padding((0., d.page_padding()))
                    .cross_align(Alignment::Center)
                    .child(brand(32., NodeStatus)),
            )
            .child(divider())
    }
}

// ---- desktop sidebar -------------------------------------------------------------------------

/// A sidebar entry.
#[derive(PartialEq)]
struct SideRow {
    label: String,
    icon: Option<Icon>,
    /// A dot instead of an icon: online or not.
    dot: Option<bool>,
    active: bool,
    badge: usize,
    height: f32,
    on_press: EventHandler<()>,
    key: DiffKey,
}

impl SideRow {
    fn button(label: impl Into<String>, icon: Icon, active: bool, on_press: impl FnMut(()) + 'static) -> Self {
        Self {
            label: label.into(),
            icon: Some(icon),
            dot: None,
            active,
            badge: 0,
            height: 36.,
            on_press: EventHandler::new(on_press),
            key: DiffKey::None,
        }
    }

    fn place(label: impl Into<String>, online: bool, active: bool, on_press: impl FnMut(()) + 'static) -> Self {
        Self {
            label: label.into(),
            icon: None,
            dot: Some(online),
            active,
            badge: 0,
            height: 40.,
            on_press: EventHandler::new(on_press),
            key: DiffKey::None,
        }
    }

    fn badge(mut self, n: usize) -> Self {
        self.badge = n;
        self
    }
}

impl KeyExt for SideRow {
    fn key_mut(&mut self) -> &mut DiffKey {
        &mut self.key
    }
}

impl Component for SideRow {
    fn render(&self) -> impl IntoElement {
        let (c, d) = look();
        let mut hovered = use_state(|| false);
        let bg = match (self.active, *hovered.read()) {
            (true, _) => c.accent_soft,
            (false, true) => c.chip_bg,
            _ => Color::TRANSPARENT,
        };
        let on_press = self.on_press.clone();
        rect()
            .horizontal()
            .width(Size::fill())
            .height(Size::px(self.height))
            .content(Content::flex())
            .corner_radius(Dim::RADIUS)
            .background(bg)
            .padding((0., Dim::SPACE3))
            .spacing(10.)
            .cross_align(Alignment::Center)
            .cursor(CursorIcon::Pointer)
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set(false))
            .on_press(move |_| on_press.call(()))
            .map(self.icon, |el, i| {
                el.child(icon(i, 18., if self.active { c.accent_text } else { c.text_muted }))
            })
            .map(self.dot, |el, online| el.child(dot(online, 8.)))
            .child(
                txt(
                    self.label.clone(),
                    d.font_md(),
                    if self.active { c.text_strong } else { c.text },
                    if self.active { 600 } else { 500 },
                )
                .width(Size::flex(1.))
                .elide(),
            )
            .maybe(self.badge > 0, |el| el.child(count_badge(self.badge, 18.)))
    }

    fn render_key(&self) -> DiffKey {
        self.key.clone().or(self.default_key())
    }
}

fn count_badge(n: usize, size: f32) -> Rect {
    let (c, _) = look();
    rect()
        .min_width(Size::px(size))
        .height(Size::px(size))
        .corner_radius(size / 2.)
        .background(c.accent)
        .padding((0., 5.))
        .center()
        .child(txt(n.to_string(), if size > 16. { 11. } else { 10. }, Color::WHITE, 700))
}

fn empty_note(text: &str) -> Label {
    let (c, d) = look();
    txt(text, d.font_sm(), c.text_faint, 400)
        .width(Size::fill())
        .text_align(TextAlign::Center)
        .padding((12., 0.))
}

#[derive(PartialEq)]
struct Sidebar;

impl Component for Sidebar {
    fn render(&self) -> impl IntoElement {
        let (c, _) = look();
        let nav = use_consume::<Nav>();
        let peers = use_radio::<Model, Channel>(Channel::Peers);
        let groups = use_radio::<Model, Channel>(Channel::Groups);
        let home = use_radio::<Model, Channel>(Channel::Home);
        let mut ui = nav.ui;
        // While the handle is dragged: where the pointer and the edge were when it started.
        let mut drag = use_state(|| None::<(f64, f32)>);
        let mut handle_hovered = use_state(|| false);

        let (page, width) = {
            let ui = ui.read();
            (ui.page.clone(), ui.sidebar_width)
        };
        let open = match &page {
            Page::Group(id) => Some(id.clone()),
            _ => None,
        };
        let m = groups.read();
        let _ = (peers.read(), home.read());

        let peer_rows: Vec<Element> = m
            .paired_peers()
            .filter_map(|p| {
                let gid = p.pair_group.clone()?;
                let active = open.as_deref() == Some(gid.as_str());
                Some(
                    SideRow::place(peer_title(p), p.connected, active, move |_| nav.open_group(&gid, None))
                        .key(p.id.to_string())
                        .into_element(),
                )
            })
            .collect();
        let invite_rows = m.groups.invites.iter().map(|i| {
            SideRow::button(format!("Invitation: {}", i.haiku), Icon::Groups, false, move |_| nav.go(Page::Home))
                .badge(1)
                .key(format!("{}{}", i.inviter, i.group))
                .into_element()
        });
        let group_rows: Vec<Element> = m
            .listed_groups()
            .map(|g| {
                let (gid, active) = (g.id.clone(), open.as_deref() == Some(g.id.as_str()));
                SideRow::place(group_title(g), m.group_online(&g.id), active, move |_| nav.open_group(&gid, None))
                    .key(g.id.clone())
                    .into_element()
            })
            .collect();
        let no_groups = group_rows.is_empty() && m.groups.invites.is_empty();
        let no_peers = peer_rows.is_empty();

        rect()
            .horizontal()
            .height(Size::fill())
            .child(
                rect()
                    .width(Size::px(width))
                    .height(Size::fill())
                    .background(c.sidebar_bg)
                    .padding(Dim::SPACE3)
                    .spacing(Dim::SPACE1)
                    .content(Content::flex())
                    .child(rect().padding(Dim::SPACE1).child(brand(34., NodeStatus)))
                    .child(rect().height(Size::px(Dim::SPACE3)))
                    .child(
                        SideRow::button("Home", Icon::Inbox, page == Page::Home, move |_| nav.go(Page::Home))
                            .badge(m.home_badge()),
                    )
                    .child(
                        ScrollView::new()
                            .width(Size::fill())
                            .height(Size::flex(1.))
                            .spacing(2.)
                            .child(rect().padding((Dim::SPACE2, 0., 0., Dim::SPACE3)).child(overline("PEERS")))
                            .children(peer_rows)
                            .maybe(no_peers, |el| el.child(empty_note("No paired peers yet")))
                            .child(
                                rect()
                                    .horizontal()
                                    .width(Size::fill())
                                    .content(Content::flex())
                                    .cross_align(Alignment::Center)
                                    .padding((Dim::SPACE2, 0., 0., Dim::SPACE3))
                                    .child(rect().width(Size::flex(1.)).child(overline("GROUPS")))
                                    .child(
                                        IconButton::new(Icon::AddPeer, move |_| nav.new_group())
                                            .tint(c.accent_text)
                                            .size(28.),
                                    ),
                            )
                            .children(invite_rows)
                            .children(group_rows)
                            .maybe(no_groups, |el| el.child(empty_note("No groups yet"))),
                    )
                    .child(SideRow::button("Peers", Icon::Peers, page == Page::Peers, move |_| nav.go(Page::Peers)))
                    .child(SideRow::button("Add a peer", Icon::AddPeer, page == Page::Add, move |_| nav.go(Page::Add)))
                    .child(SideRow::button("Guest link", Icon::Link, false, move |_| nav.new_guest_link()))
                    .child(SideRow::button("Settings", Icon::Settings, page == Page::Settings, move |_| {
                        nav.go(Page::Settings)
                    })),
            )
            // The divider doubles as a resize handle: a wide invisible strip around a hairline.
            .child(
                rect()
                    .width(Size::px(7.))
                    .height(Size::fill())
                    .center()
                    .cursor(CursorIcon::EwResize)
                    .on_pointer_enter(move |_| handle_hovered.set(true))
                    .on_pointer_leave(move |_| handle_hovered.set(false))
                    .on_pointer_down(move |e: Event<PointerEventData>| {
                        drag.set(Some((e.global_location().x, ui.peek().sidebar_width)));
                    })
                    .on_global_pointer_move(move |e: Event<PointerEventData>| {
                        if let Some((x, start)) = *drag.peek() {
                            let width = (start + (e.global_location().x - x) as f32)
                                .clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
                            ui.write().sidebar_width = width;
                        }
                    })
                    .on_global_pointer_up(move |_| {
                        if drag.peek().is_some() {
                            drag.set(None);
                            nav.save_sidebar_width();
                        }
                    })
                    .child(
                        rect()
                            .width(Size::px(1.))
                            .height(Size::fill())
                            .background(if *handle_hovered.read() || drag.read().is_some() {
                                c.border_strong
                            } else {
                                c.border
                            }),
                    ),
            )
    }
}

// ---- phone bottom navigation -----------------------------------------------------------------

#[derive(PartialEq)]
struct NavTab {
    label: &'static str,
    icon: Icon,
    active: bool,
    badge: usize,
    on_press: EventHandler<()>,
}

impl Component for NavTab {
    fn render(&self) -> impl IntoElement {
        let (c, d) = look();
        let on_press = self.on_press.clone();
        rect()
            .width(Size::flex(1.))
            .height(Size::fill())
            .center()
            .spacing(4.)
            .on_press(move |_| on_press.call(()))
            .child(
                rect()
                    .width(Size::px(60.))
                    .height(Size::px(32.))
                    .corner_radius(16.)
                    .background(if self.active { c.accent_soft } else { Color::TRANSPARENT })
                    .center()
                    .child(icon(self.icon, 22., if self.active { c.accent_text } else { c.text_muted }))
                    .maybe(self.badge > 0, |el| {
                        el.child(
                            rect()
                                .position(Position::new_absolute().top(0.).left(36.))
                                .child(count_badge(self.badge, 16.)),
                        )
                    }),
            )
            .child(
                txt(
                    self.label,
                    d.font_xs(),
                    if self.active { c.text_strong } else { c.text_muted },
                    if self.active { 700 } else { 500 },
                )
            )
    }
}

#[derive(PartialEq)]
struct BottomNav;

impl Component for BottomNav {
    fn render(&self) -> impl IntoElement {
        let (c, _) = look();
        let nav = use_consume::<Nav>();
        let peers = use_radio::<Model, Channel>(Channel::Peers);
        let groups = use_radio::<Model, Channel>(Channel::Groups);
        let home = use_radio::<Model, Channel>(Channel::Home);
        let _ = (peers.read(), home.read());
        let m = groups.read();
        let page = nav.ui.read().page.clone();
        // A paired peer's page belongs with the peers, any other group's with the groups.
        let implied = match &page {
            Page::Group(id) => m.group(id).is_some_and(|g| g.with.is_some()),
            _ => false,
        };
        let in_group = matches!(page, Page::Group(_));
        rect()
            .width(Size::fill())
            .background(c.window_bg)
            .child(divider())
            .child(
                rect()
                    .horizontal()
                    .width(Size::fill())
                    .height(Size::px(64.))
                    .content(Content::flex())
                    .child(NavTab {
                        label: "Home",
                        icon: Icon::Inbox,
                        active: page == Page::Home,
                        badge: m.home_badge(),
                        on_press: EventHandler::new(move |_| nav.go(Page::Home)),
                    })
                    .child(NavTab {
                        label: "Groups",
                        icon: Icon::Groups,
                        active: page == Page::Groups || (in_group && !implied),
                        badge: 0,
                        on_press: EventHandler::new(move |_| nav.go(Page::Groups)),
                    })
                    .child(NavTab {
                        label: "Peers",
                        icon: Icon::Peers,
                        active: page == Page::Peers || page == Page::Add || implied,
                        badge: 0,
                        on_press: EventHandler::new(move |_| nav.go(Page::Peers)),
                    })
                    .child(NavTab {
                        label: "Settings",
                        icon: Icon::Settings,
                        active: page == Page::Settings,
                        badge: 0,
                        on_press: EventHandler::new(move |_| nav.go(Page::Settings)),
                    }),
            )
    }
}

// ---- dialogs ---------------------------------------------------------------------------------

/// A bottom sheet over most of the window, with a title row and a scrolling list.
fn list_sheet(title: impl IntoElement, on_close: impl FnMut(()) + 'static, list: Vec<Element>, empty: String) -> Rect {
    let (c, d) = look();
    let empty_list = list.is_empty();
    sheet()
        .width(Size::fill())
        .height(Size::fill())
        .margin((80., 0., 0., 0.))
        .corner_radius(CornerRadius::new(Dim::RADIUS_LG, Dim::RADIUS_LG, 0., 0.))
        .background(c.window_bg)
        .padding(d.page_padding())
        .spacing(Dim::SPACE3)
        .content(Content::flex())
        .child(
            rect()
                .horizontal()
                .width(Size::fill())
                .content(Content::flex())
                .cross_align(Alignment::Center)
                .child(rect().width(Size::flex(1.)).child(title))
                .child(IconButton::new(Icon::Close, on_close)),
        )
        .child(
            ScrollView::new()
                .width(Size::fill())
                .height(Size::flex(1.))
                .spacing(Dim::SPACE2)
                .children(list)
                .maybe(empty_list, |el| el.child(hint_center(empty))),
        )
}

/// Rename / delete a peer or a group.
#[derive(PartialEq)]
struct EditDialog;

impl Component for EditDialog {
    fn render(&self) -> impl IntoElement {
        let (c, d) = look();
        let nav = use_consume::<Nav>();
        let mut ui = nav.ui;
        let Some(edit) = ui.read().edit.clone() else {
            return rect();
        };
        let backend = nav.backend();
        let save = {
            let (backend, edit) = (backend.clone(), edit.clone());
            move || {
                let edit = ui.peek().edit.clone().unwrap_or(edit.clone());
                backend.run(|core| {
                    match edit.kind {
                        EditKind::Peer => {
                            if let Ok(id) = edit.id.parse() {
                                core.rename_contact(id, &edit.text)
                            }
                        }
                        EditKind::Group => core.rename_group(&edit.id, &edit.text),
                    }
                    Ok(())
                });
                nav.edit(None);
            }
        };
        let remove = {
            let edit = edit.clone();
            move |_| {
                backend.run(|core| {
                    match edit.kind {
                        EditKind::Peer => {
                            if let Ok(id) = edit.id.parse() {
                                core.forget_contact(id);
                                core.disconnect(id);
                            }
                        }
                        EditKind::Group => {
                            nav.close_group();
                            core.delete_group(&edit.id);
                        }
                    }
                    Ok(())
                });
                nav.edit(None);
            }
        };
        let on_submit = save.clone();
        let on_save = save;
        scrim(
            move |_| nav.edit(None),
            sheet()
                .margin((80., 16., 0., 16.))
                .width(Size::fill())
                .max_width(Size::px(400.))
                .corner_radius(Dim::RADIUS_LG)
                .background(c.card_bg)
                .border(Border::new().fill(c.border_strong).width(1.).alignment(BorderAlignment::Inner))
                .padding(d.card_padding())
                .spacing(Dim::SPACE2)
                .child(txt(
                    if edit.kind == EditKind::Peer { "Rename peer" } else { "Rename group" },
                    d.font_lg(),
                    c.text_strong,
                    700,
                ))
                .child(txt(edit.original.clone(), d.font_sm(), c.text_muted, 400))
                .child(
                    field(edit.text.clone(), "Local name (empty restores the original)", move |text| {
                        if let Some(edit) = &mut ui.write().edit {
                            edit.text = text;
                        }
                    })
                    .auto_focus(true)
                    .on_submit(move |_| on_submit()),
                )
                .child(
                    rect()
                        .horizontal()
                        .spacing(Dim::SPACE2)
                        .child(Btn::primary("Save").on_press(move |_| on_save()))
                        .child(Btn::secondary("Cancel").on_press(move |_| nav.edit(None)))
                        .child(
                            Btn::danger(if edit.kind == EditKind::Peer { "Remove" } else { "Leave" })
                                .on_press(remove),
                        ),
                ),
        )
    }
}

/// Installed apps to share (Android).
#[derive(PartialEq)]
struct AppPicker;

impl Component for AppPicker {
    fn render(&self) -> impl IntoElement {
        let (c, d) = look();
        let nav = use_consume::<Nav>();
        let mut ui = nav.ui;
        let apps = ui.read().apps.clone().unwrap_or_default();
        let gid = ui.read().open_group().map(str::to_owned).unwrap_or_default();
        let list = apps
            .into_iter()
            .map(|(path, name, detail)| {
                let (gid, label) = (gid.clone(), name.clone());
                ClickCard::new(move |_| {
                    ui.write().apps = None;
                    nav.backend().share_app(gid.clone(), path.clone(), name.clone());
                })
                .child(
                    card_body(2.)
                        .padding(Dim::SPACE3)
                        .child(txt(label, d.font_md(), c.text_strong, 650).elide())
                        .child(txt(detail, d.font_xs(), c.text_muted, 400).elide()),
                )
                .into_element()
            })
            .collect();
        scrim(
            move |_| ui.write().apps = None,
            list_sheet(
                txt("Share an app", d.font_xl(), c.text_strong, 700),
                move |_| ui.write().apps = None,
                list,
                "No apps found.".into(),
            ),
        )
    }
}

/// Where to send what another app shared with us (Android) or was dropped on the window (desktop):
/// online groups and peers first, fully offline ones last.
#[derive(PartialEq)]
struct SharePicker;

impl Component for SharePicker {
    fn render(&self) -> impl IntoElement {
        let (c, d) = look();
        let nav = use_consume::<Nav>();
        let mut ui = nav.ui;
        let me = use_radio::<Model, Channel>(Channel::Me);
        let peers = use_radio::<Model, Channel>(Channel::Peers);
        let groups = use_radio::<Model, Channel>(Channel::Groups);
        let _ = peers.read();
        let ready = me.read().core.is_some();
        let Some(share) = ui.read().share.clone() else {
            return rect();
        };
        let m = groups.read();
        // (group, title, detail, online, peer)
        let mut targets: Vec<(String, String, String, bool, bool)> = m
            .paired_peers()
            .filter_map(|p| {
                let detail = if p.connected { "Paired peer · connected" } else { "Paired peer · offline" };
                Some((p.pair_group.clone()?, peer_title(p), detail.to_owned(), p.connected, true))
            })
            .chain(
                m.listed_groups()
                    .map(|g| (g.id.clone(), group_title(g), m.group_summary(&g.id), m.group_online(&g.id), false)),
            )
            .collect();
        // Stable: peers before groups within each half, each in its usual order.
        targets.sort_by_key(|t| !t.3);
        let list = targets
            .into_iter()
            .map(|(gid, title, detail, online, peer)| {
                ClickCard::new(move |_| nav.share_to(&gid))
                    .border(if peer { c.peer_border } else { c.group_border }, 2.)
                    .child(
                        rect()
                            .horizontal()
                            .width(Size::fill())
                            .content(Content::flex())
                            .padding(Dim::SPACE3)
                            .spacing(Dim::SPACE3)
                            .cross_align(Alignment::Center)
                            .child(dot_colored(if online { c.online } else { c.error }, 10.))
                            .child(icon(if peer { Icon::Person } else { Icon::Groups }, 18., c.text_muted))
                            .child(
                                rect()
                                    .width(Size::flex(1.))
                                    .spacing(2.)
                                    .child(txt(title, d.font_md(), c.text_strong, 650).elide())
                                    .child(txt(detail, d.font_xs(), c.text_muted, 400).elide()),
                            ),
                    )
                    .into_element()
            })
            .collect();
        let text = share.text().to_owned();
        let empty = if ready {
            "No groups or paired peers yet. Create a group or pair with a peer first."
        } else {
            "Starting…"
        };
        scrim(
            move |_| ui.write().share = None,
            list_sheet(
                rect()
                    .spacing(2.)
                    .child(txt(format!("Share {}", share.what()), d.font_xl(), c.text_strong, 700))
                    .maybe(!text.is_empty(), |el| {
                        el.child(txt(text, d.font_sm(), c.text_muted, 400).elide())
                    }),
                move |_| ui.write().share = None,
                list,
                empty.into(),
            ),
        )
    }
}

/// Files dragged over the window (desktop): dropping them sends them to the group showing, or asks where they go.
#[derive(PartialEq)]
struct DropHint;

impl Component for DropHint {
    fn render(&self) -> impl IntoElement {
        let (c, d) = look();
        let nav = use_consume::<Nav>();
        let groups = use_radio::<Model, Channel>(Channel::Groups);
        let m = groups.read();
        let target = nav.ui.read().open_group().and_then(|id| m.group(id)).map(group_title);
        rect()
            .layer(Layer::Overlay)
            .position(Position::new_global())
            .width(Size::window_percent(100.))
            .height(Size::window_percent(100.))
            .background(Color::BLACK.with_a(140))
            .padding(d.page_padding())
            .child(
                rect()
                    .expanded()
                    .corner_radius(Dim::RADIUS_LG)
                    .background(c.window_bg.with_a(235))
                    .border(Border::new().fill(c.accent).width(2.).alignment(BorderAlignment::Inner))
                    .center()
                    .spacing(Dim::SPACE3)
                    .child(icon(Icon::Share, 40., c.accent))
                    .map(target, |el, group| {
                        el.child(
                            txt(format!("Drop to share with {group}"), d.font_xl(), c.text_strong, 700)
                                .width(Size::fill())
                                .text_align(TextAlign::Center),
                        )
                    })
                    .maybe(nav.ui.read().open_group().is_none(), |el| {
                        el.child(txt("Drop to share", d.font_xl(), c.text_strong, 700)).child(txt(
                            "Then pick a group or peer to send it to",
                            d.font_sm(),
                            c.text_muted,
                            400,
                        ))
                    }),
            )
    }
}
