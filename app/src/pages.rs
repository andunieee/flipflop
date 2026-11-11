//! The pages that are not a group's: Home, Groups, Peers, Add a peer, Settings, and the webcam view.

use flipflop::core::{GroupInfo, InviteInfo, Peer, TransferItem, Unseen};
use freya::{elements::image::ImageHandle, engine::prelude::AlphaType, prelude::*, radio::use_radio};

use crate::{
    icons::Icon,
    model::{Channel, Model, Tone, group_title, pair_request, peer_subtitle, peer_title, transfer_progress},
    nav::{Edit, EditKind, Nav, Page},
    theme::{Dim, look},
    widgets::*,
};

/// A page whose list scrolls under a fixed header.
pub fn list_page(header: impl IntoElement, items: Vec<Element>) -> Rect {
    let (_, d) = look();
    let p = d.page_padding();
    rect()
        .expanded()
        .padding((p, p, Dim::SPACE3, p))
        .spacing(Dim::SPACE3)
        .content(Content::flex())
        .child(header)
        .child(
            ScrollView::new()
                .width(Size::fill())
                .height(Size::flex(1.))
                .spacing(Dim::SPACE3)
                .children(items),
        )
}

/// A padded page that scrolls as a whole.
pub fn scroll_page(items: Vec<Element>) -> ScrollView {
    let (_, d) = look();
    ScrollView::new().expanded().child(
        rect()
            .width(Size::fill())
            .padding(d.page_padding())
            .spacing(Dim::SPACE3)
            .children(items),
    )
}

/// A title, its explanation and an action on the right.
pub fn header_row(title: &str, subtitle: &str, action: impl IntoElement) -> Rect {
    rect()
        .horizontal()
        .width(Size::fill())
        .content(Content::flex())
        .spacing(Dim::SPACE3)
        .child(page_title(title, subtitle))
        .child(action)
}

/// Put `text` on the clipboard, and say so.
pub fn copy(nav: Nav, text: &str) {
    flipflop::platform::copy_text(text);
    nav.toast("Copied".into());
}

// ---- Home ------------------------------------------------------------------------------------

fn invite_card(invite: &InviteInfo, nav: Nav) -> Element {
    let (c, d) = look();
    let (inviter, group) = (invite.inviter, invite.group.clone());
    let decline = group.clone();
    card_with(c.accent_soft, c.accent_text.with_a(100), 1.)
        .key(format!("invite{inviter}{group}"))
        .child(
            card_body(Dim::SPACE2)
                .child(txt(format!("{} invites you to join", invite.inviter_name), d.font_sm(), c.text_muted, 400).elide())
                .child(txt(invite.haiku.clone(), d.font_lg(), c.text_strong, 700))
                .child(
                    rect()
                        .horizontal()
                        .spacing(Dim::SPACE2)
                        .child(Btn::primary("Accept").on_press(move |_| {
                            nav.backend().run(|core| Ok(core.accept_invite(inviter, &group)))
                        }))
                        .child(Btn::secondary("Decline").on_press(move |_| {
                            nav.backend().run(|core| Ok(core.reject_invite(inviter, &decline)))
                        })),
                ),
        )
        .into_element()
}

/// A peer that paired with us, waiting for us to pair back.
fn pair_request_card(peer: &Peer, nav: Nav) -> Element {
    let (c, d) = look();
    let id = peer.id;
    card_with(c.accent_soft, c.accent_text.with_a(100), 1.)
        .key(format!("pair{id}"))
        .child(
            card_body(Dim::SPACE2)
                .child(
                    txt(
                        format!("Wants to pair with you{}", if peer.connected { "" } else { " · offline" }),
                        d.font_sm(),
                        c.text_muted,
                        400,
                    )
                    .elide(),
                )
                .child(txt(peer_title(peer), d.font_lg(), c.text_strong, 700).elide())
                .child(txt(peer_subtitle(peer), d.font_xs(), c.text_faint, 400).elide())
                .child(
                    rect()
                        .horizontal()
                        .spacing(Dim::SPACE2)
                        .child(
                            Btn::primary("Pair")
                                .icon(Icon::AddPeer)
                                .on_press(move |_| nav.backend().run(|core| Ok(core.pair(id)))),
                        )
                        .child(
                            Btn::secondary("Ignore")
                                .on_press(move |_| nav.backend().run(|core| Ok(core.ignore_pair(id)))),
                        ),
                ),
        )
        .into_element()
}

/// A row inside a clickable card: content, then a chevron.
fn card_row(content: impl IntoElement) -> Rect {
    let (c, d) = look();
    rect()
        .horizontal()
        .width(Size::fill())
        .content(Content::flex())
        .padding(d.card_padding())
        .spacing(Dim::SPACE3)
        .cross_align(Alignment::Center)
        .child(content)
        .child(icon(Icon::ChevronRight, 18., c.text_faint))
}

fn home_card(item: &Unseen, nav: Nav) -> Element {
    let (c, d) = look();
    let (gid, tab) = (item.group.clone(), item.tab());
    ClickCard::new(move |_| nav.open_group(&gid, Some(tab)))
        .key(format!("news{}", item.group))
        .child(card_row(
            rect()
                .width(Size::flex(1.))
                .spacing(2.)
                .child(txt(item.title.clone(), d.font_lg(), c.text_strong, 650))
                .child(txt(item.lines().join("\n"), d.font_sm(), c.text, 400)),
        ))
        .into_element()
}

/// A file moving (or waiting its turn): its progress, opening the group's files.
fn transfer_card(t: &TransferItem, detail: String, nav: Nav) -> Element {
    let (c, d) = look();
    let gid = t.group.clone();
    let tile = match t.kind.as_str() {
        "upload" => Icon::Send,
        "receive" => Icon::Inbox,
        "prepare" => Icon::File,
        _ => Icon::ArrowDown,
    };
    let progress = transfer_progress(t);
    ClickCard::new(move |_| nav.open_group(&gid, Some("files")))
        .key(format!("transfer{}{}{}", t.group, t.kind, t.name))
        .child(card_row(
            rect()
                .horizontal()
                .width(Size::flex(1.))
                .content(Content::flex())
                .spacing(Dim::SPACE3)
                .cross_align(Alignment::Center)
                .child(icon_tile(tile, 40., c.accent_soft, c.accent_text))
                .child(
                    rect()
                        .width(Size::flex(1.))
                        .spacing(4.)
                        .child(txt(t.name.clone(), d.font_md(), c.text_strong, 650).elide())
                        .child(txt(detail, d.font_sm(), c.text_muted, 400).elide())
                        .map(progress, |el, p| el.child(progress_bar(p))),
                )
                .maybe(t.queued, |el| el.child(badge("Queued", Tone::Muted))),
        ))
        .into_element()
}

/// A paired peer that is online: its summary, opening its page.
fn online_peer_card(peer: &Peer, nav: Nav) -> Option<Element> {
    let (c, _) = look();
    let gid = peer.pair_group.clone()?;
    Some(
        ClickCard::new(move |_| nav.open_group(&gid, None))
            .border(c.peer_border, 2.)
            .key(format!("online{}", peer.id))
            .child(card_row(
                rect()
                    .horizontal()
                    .width(Size::flex(1.))
                    .content(Content::flex())
                    .spacing(Dim::SPACE3)
                    .cross_align(Alignment::Center)
                    .child(icon(Icon::Person, 18., c.text_muted))
                    .child(peer_summary(peer, true)),
            ))
            .into_element(),
    )
}

#[derive(PartialEq)]
pub struct HomePage;

impl Component for HomePage {
    fn render(&self) -> impl IntoElement {
        let nav = use_consume::<Nav>();
        let peers = use_radio::<Model, Channel>(Channel::Peers);
        let groups = use_radio::<Model, Channel>(Channel::Groups);
        let home = use_radio::<Model, Channel>(Channel::Home);
        let _ = (peers.read(), groups.read());
        let m = home.read();

        let has_news = m.news().next().is_some();
        let mut items: Vec<Element> = vec![];
        items.extend(m.peers.iter().filter(|p| pair_request(p)).map(|p| pair_request_card(p, nav)));
        items.extend(m.groups.invites.iter().map(|i| invite_card(i, nav)));
        if !m.transfers.is_empty() {
            items.push(overline("TRANSFERS").into_element());
        }
        items.extend(m.transfers.iter().map(|t| transfer_card(t, m.transfer_detail(t), nav)));
        if has_news {
            items.push(overline("NEW").into_element());
        }
        items.extend(m.news().map(|n| home_card(n, nav)));
        let online_peers: Vec<Element> = m
            .paired_peers()
            .filter(|p| p.connected)
            .filter_map(|p| online_peer_card(p, nav))
            .collect();
        let online_groups: Vec<Element> = m
            .listed_groups()
            .filter(|g| m.group_online(&g.id))
            .map(|g| group_card(g, &m, nav, true))
            .collect();
        if !online_peers.is_empty() || !online_groups.is_empty() {
            items.push(overline("ONLINE NOW").into_element());
        }
        items.extend(online_peers);
        items.extend(online_groups);
        if items.is_empty() {
            items.push(hint_center("All caught up. New files, texts and members of your groups show up here.").into_element());
        }

        list_page(
            header_row(
                "Home",
                "News and online peers.",
                rect().maybe(has_news, |el| {
                    el.child(Btn::secondary("Mark all as seen").on_press(move |_| {
                        if let Some(core) = nav.backend().core() {
                            core.mark_all_seen();
                        }
                    }))
                }),
            ),
            items,
        )
    }
}

// ---- Groups ----------------------------------------------------------------------------------

/// `on_home`: listed under "online now", so the online dot goes without saying.
fn group_card(g: &GroupInfo, m: &Model, nav: Nav, on_home: bool) -> Element {
    let (c, d) = look();
    let gid = g.id.clone();
    let subtitle = if g.alias.is_some() { g.haiku.clone() } else { String::new() };
    ClickCard::new(move |_| nav.open_group(&gid, None))
        .border(c.group_border, 2.)
        .key(format!("group{}", g.id))
        .child(card_row(
            rect()
                .horizontal()
                .width(Size::flex(1.))
                .content(Content::flex())
                .spacing(Dim::SPACE3)
                .cross_align(Alignment::Center)
                .child(icon(Icon::Groups, 18., c.text_muted))
                .child(
                    rect()
                        .width(Size::flex(1.))
                        .spacing(2.)
                        .child(
                            rect()
                                .horizontal()
                                .width(Size::fill())
                                .content(Content::flex())
                                .spacing(Dim::SPACE2)
                                .cross_align(Alignment::Center)
                                .maybe(!on_home, |el| el.child(dot(m.group_online(&g.id), 8.)))
                                .child(txt(group_title(g), d.font_lg(), c.text_strong, 650).width(Size::flex(1.)))
                                .maybe(g.guest.is_some(), |el| el.child(badge("guest link", Tone::Active))),
                        )
                        .maybe(!subtitle.is_empty(), |el| el.child(txt(subtitle, d.font_sm(), c.text_muted, 400)))
                        .child(txt(m.group_summary(&g.id), d.font_sm(), c.text_muted, 400)),
                ),
        ))
        .into_element()
}

#[derive(PartialEq)]
pub struct GroupsPage;

impl Component for GroupsPage {
    fn render(&self) -> impl IntoElement {
        let nav = use_consume::<Nav>();
        let groups = use_radio::<Model, Channel>(Channel::Groups);
        let m = groups.read();
        let mut items: Vec<Element> = m.groups.invites.iter().map(|i| invite_card(i, nav)).collect();
        items.extend(m.listed_groups().map(|g| group_card(g, &m, nav, false)));
        if items.is_empty() {
            items.push(hint_center("No groups yet.").into_element());
        }
        list_page(
            header_row(
                "Groups",
                "",
                rect()
                    .horizontal()
                    .spacing(Dim::SPACE2)
                    .child(Btn::secondary("Guest link").icon(Icon::Link).on_press(move |_| nav.new_guest_link()))
                    .child(Btn::primary("New group").icon(Icon::AddPeer).on_press(move |_| nav.new_group())),
            ),
            items,
        )
    }
}

// ---- Peers -----------------------------------------------------------------------------------

/// `on_home`: listed under "online now", so "paired" and "connected" go without saying.
pub fn peer_badges(p: &Peer, on_home: bool) -> Rect {
    let paired = p.pair_group.is_some();
    rect()
        .horizontal()
        .spacing(4.)
        .maybe(paired && !on_home, |el| el.child(badge("paired", Tone::Ok)))
        .maybe(!paired && p.paired_us && !p.paired, |el| el.child(badge("wants to pair", Tone::Warn)))
        .maybe(!paired && p.paired && !p.paired_us, |el| el.child(badge("waiting for them", Tone::Muted)))
        .maybe(p.connected && !on_home, |el| el.child(badge("connected", Tone::Ok)))
        .maybe(p.saved && !p.connected, |el| el.child(badge("offline", Tone::Muted)))
        .maybe(p.lan, |el| el.child(badge("LAN", Tone::Active)))
        .maybe(p.ble, |el| el.child(badge("Bluetooth", Tone::Warn)))
}

/// A peer's name, own name and key prefix, and badges.
fn peer_summary(p: &Peer, on_home: bool) -> Rect {
    let (c, d) = look();
    rect()
        .width(Size::flex(1.))
        .spacing(3.)
        .child(txt(peer_title(p), d.font_lg(), c.text_strong, 650).elide())
        .child(txt(peer_subtitle(p), d.font_xs(), c.text_faint, 400).elide())
        .child(peer_badges(p, on_home))
}

/// The rename dialog for a peer.
pub fn rename_peer(p: &Peer) -> Edit {
    Edit {
        kind: EditKind::Peer,
        id: p.id.to_string(),
        original: p.name.clone(),
        text: p.alias.clone().unwrap_or_default(),
    }
}

fn peer_card(p: &Peer, nav: Nav) -> Element {
    let (c, d) = look();
    let id = p.id;
    let edit = rename_peer(p);
    let buttons = rect()
        .spacing(Dim::SPACE2)
        .maybe(!p.paired, |el| {
            el.child(
                Btn::primary("Pair")
                    .icon(Icon::AddPeer)
                    .on_press(move |_| nav.backend().run(|core| Ok(core.pair(id)))),
            )
        })
        .maybe(p.paired && p.pair_group.is_none(), |el| {
            el.child(Btn::ghost("Unpair").on_press(move |_| nav.backend().run(|core| Ok(core.unpair(id)))))
        })
        .maybe(!p.connected, |el| {
            el.child(Btn::secondary("Connect").on_press(move |_| nav.backend().connect(id)))
        })
        .maybe(p.saved, |el| {
            el.child(Btn::ghost("Edit").icon(Icon::Edit).on_press(move |_| nav.edit(Some(edit.clone()))))
        });
    let body = rect()
        .horizontal()
        .width(Size::fill())
        .content(Content::flex())
        .padding(d.card_padding())
        .spacing(Dim::SPACE3)
        .cross_align(Alignment::Center)
        .child(icon(Icon::Person, 18., c.text_muted))
        .child(peer_summary(p, false))
        .child(buttons);
    match p.pair_group.clone() {
        Some(gid) => ClickCard::new(move |_| nav.open_group(&gid, None))
            .border(c.peer_border, 2.)
            .key(p.id.to_string())
            .child(body)
            .into_element(),
        None => card_with(c.card_bg, c.peer_border, 2.)
            .key(p.id.to_string())
            .child(body)
            .into_element(),
    }
}

#[derive(PartialEq)]
pub struct PeersPage;

impl Component for PeersPage {
    fn render(&self) -> impl IntoElement {
        let nav = use_consume::<Nav>();
        let peers = use_radio::<Model, Channel>(Channel::Peers);
        let m = peers.read();
        let mut items: Vec<Element> = m.peers.iter().map(|p| peer_card(p, nav)).collect();
        if items.is_empty() {
            items.push(
                hint_center("No one around yet. Other FlipFlop devices on this network or in Bluetooth range show up here.")
                    .into_element(),
            );
        }
        list_page(
            header_row(
                "Peers",
                "Devices nearby and the ones you saved. Pair with a peer, and when it pairs back you get a page of your own with it.",
                Btn::primary("Add").icon(Icon::AddPeer).on_press(move |_| nav.go(Page::Add)),
            ),
            items,
        )
    }
}

// ---- Add a peer ------------------------------------------------------------------------------

#[derive(PartialEq)]
pub struct AddPage;

impl Component for AddPage {
    fn render(&self) -> impl IntoElement {
        let (c, d) = look();
        let nav = use_consume::<Nav>();
        let me = use_radio::<Model, Channel>(Channel::Me);
        let pasted = use_state(String::new);
        let ticket = me.read().ticket.clone();
        let width = Platform::get().current_window().root_size.read().width as f32;
        let qr_size = (width - 2. * d.page_padding() - 2. * d.card_padding()).clamp(120., 260.);
        let connect = move || {
            let text = pasted.peek().trim().to_owned();
            if !text.is_empty() {
                nav.backend().connect_ticket(text);
            }
        };
        let on_connect = connect.clone();
        let copied = ticket.clone();
        scroll_page(vec![
            page_title("Add a peer", "Scan a code from the other device, or show yours.").into_element(),
            card()
                .child(
                    card_body(Dim::SPACE3)
                        .child(txt("Your code", d.font_md(), c.text_strong, 650))
                        .child(
                            rect()
                                .width(Size::fill())
                                .cross_align(Alignment::Center)
                                .maybe(!ticket.is_empty(), |el| el.child(qr_code(&ticket, qr_size))),
                        )
                        .child(Btn::primary("Scan a code").icon(Icon::Qr).on_press(move |_| nav.backend().scan())),
                )
                .into_element(),
            card()
                .child(
                    card_body(Dim::SPACE2)
                        .child(txt("…or paste a ticket", d.font_md(), c.text_strong, 650))
                        .child(
                            Input::new(pasted)
                                .placeholder("paste the other device's ticket")
                                .width(Size::fill())
                                .on_submit(move |_| connect()),
                        )
                        .child(
                            Btn::secondary("Connect")
                                .enabled(!pasted.read().is_empty())
                                .on_press(move |_| on_connect()),
                        ),
                )
                .into_element(),
            card()
                .child(
                    card_body(Dim::SPACE2)
                        .child(txt("Your ticket", d.font_md(), c.text_strong, 650))
                        .child(copyable(ticket))
                        .child(Btn::secondary("Copy").icon(Icon::Copy).on_press(move |_| copy(nav, &copied))),
                )
                .into_element(),
        ])
    }
}

/// The webcam preview while scanning (desktop).
#[derive(PartialEq)]
pub struct ScanView;

impl Component for ScanView {
    fn render(&self) -> impl IntoElement {
        let (c, d) = look();
        let nav = use_consume::<Nav>();
        let mut frame = use_state(|| None::<ImageHandle>);
        use_hook(move || {
            let mut frames = nav.backend().frames();
            spawn(async move {
                while frames.changed().await.is_ok() {
                    let latest = frames.borrow_and_update().clone();
                    frame.set(latest.and_then(|(w, h, rgba)| {
                        ImageHandle::from_rgba(w, h, rgba, AlphaType::Opaque)
                    }));
                }
            });
        });
        rect()
            .expanded()
            .padding(d.page_padding())
            .spacing(Dim::SPACE3)
            .content(Content::flex())
            .child(
                txt("Point the camera at a FlipFlop code", d.font_md(), c.text_strong, 400)
                    .width(Size::fill())
                    .text_align(TextAlign::Center),
            )
            .child(
                rect()
                    .width(Size::fill())
                    .height(Size::flex(1.))
                    .center()
                    .map(frame.read().clone(), |el, handle| {
                        el.child(
                            image(handle)
                                .width(Size::fill())
                                .height(Size::fill())
                                .aspect_ratio(AspectRatio::Min),
                        )
                    }),
            )
            .child(Btn::secondary("Cancel").on_press(move |_| nav.backend().cancel_scan()))
    }
}

// ---- Settings --------------------------------------------------------------------------------

/// This device's name, to edit. Starts over when the name changes elsewhere (the command line).
#[derive(PartialEq)]
struct NameCard {
    name: String,
}

impl Component for NameCard {
    fn render(&self) -> impl IntoElement {
        let (c, d) = look();
        let nav = use_consume::<Nav>();
        let name = use_state(|| self.name.clone());
        let save = move || {
            let text = name.peek().clone();
            nav.backend().run(|core| Ok(core.set_name(&text)));
        };
        let on_save = save.clone();
        card().child(
            card_body(Dim::SPACE2)
                .child(txt("This device's name", d.font_md(), c.text_strong, 650))
                .child(hint("Shown to other members next to the texts you send."))
                .child(
                    Input::new(name)
                        .placeholder("Device name")
                        .width(Size::fill())
                        .on_submit(move |_| save()),
                )
                .child(Btn::primary("Save").on_press(move |_| on_save())),
        )
    }

    fn render_key(&self) -> DiffKey {
        DiffKey::from(&self.name)
    }
}

#[derive(PartialEq)]
pub struct SettingsPage;

impl Component for SettingsPage {
    fn render(&self) -> impl IntoElement {
        let (c, d) = look();
        let nav = use_consume::<Nav>();
        let me = use_radio::<Model, Channel>(Channel::Me);
        let (my_name, download_dir, ble_status) = {
            let m = me.read();
            (m.name.clone(), m.download_dir.clone(), m.ble_status.clone())
        };
        let theme = nav.ui.read().theme.clone();
        let section = |title: &str| txt(title, d.font_md(), c.text_strong, 650);
        scroll_page(vec![
            page_title("Settings", "").into_element(),
            NameCard { name: my_name }.into_element(),
            card()
                .child(
                    card_body(Dim::SPACE2).child(section("Appearance")).child(TabStrip::new(
                        vec![
                            ("system".into(), "System".into()),
                            ("light".into(), "Light".into()),
                            ("dark".into(), "Dark".into()),
                        ],
                        theme,
                        move |t: String| nav.set_theme(&t),
                    )),
                )
                .into_element(),
            card()
                .child(
                    card_body(Dim::SPACE2)
                        .child(section("Downloads"))
                        .child(txt(download_dir, d.font_sm(), c.text_muted, 400)),
                )
                .into_element(),
            card()
                .child(
                    card_body(Dim::SPACE2).child(section("Connection")).child(txt(
                        if ble_status.is_empty() { "Starting…".to_owned() } else { ble_status },
                        d.font_sm(),
                        c.text_muted,
                        400,
                    )),
                )
                .into_element(),
        ])
    }
}
