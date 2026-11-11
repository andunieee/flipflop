//! One open group (or paired peer's page): its header, tabs, and the tab showing.

use flipflop::{
    core::{FileView, GroupInfo, GroupView, Peer, TabView, TextView},
    format,
};
use freya::{prelude::*, radio::use_radio};

use crate::{
    icons::Icon,
    model::{Channel, Model, Tone, group_title, peer_subtitle, peer_title},
    nav::{Edit, EditKind, Nav},
    pad::PadEditor,
    pages::{copy, rename_peer},
    theme::{Dim, look},
    widgets::*,
};

/// Files no longer offered that the files tab shows before "Show more".
const PAST_FILES: usize = 10;

#[derive(PartialEq)]
pub struct GroupPage {
    pub id: String,
}

impl Component for GroupPage {
    fn render(&self) -> impl IntoElement {
        let (c, d) = look();
        let nav = use_consume::<Nav>();
        let groups = use_radio::<Model, Channel>(Channel::Groups);
        let m = groups.read();
        let Some(info) = m.group(&self.id) else {
            return rect().into_element();
        };
        let view = m.views.get(&self.id).cloned().unwrap_or_default();
        let shown: Vec<&TabView> = view.tabs.iter().filter(|t| t.on).collect();
        let first = shown.first().map_or("info".to_owned(), |t| t.id.clone());
        let asked = nav.ui.read().group_tab.clone();
        // The tab asked for may be hidden (by anyone in the group): show the first one there is.
        let tab = if asked == "info" || shown.iter().any(|t| t.id == asked) { asked.clone() } else { first.clone() };
        let p = d.page_padding();

        let content = match tab.as_str() {
            "files" => FilesTab { gid: self.id.clone(), files: view.files.clone() }.into_element(),
            "texts" => TextsTab { gid: self.id.clone(), texts: view.texts.clone() }.into_element(),
            "pad" => rect()
                .expanded()
                .spacing(Dim::SPACE2)
                .content(Content::flex())
                .child(PadEditor { gid: self.id.clone(), text: view.scratch.clone(), local: view.scratch_local })
                .child(pad_actions(view.scratch.clone(), nav, self.id.clone()))
                .into_element(),
            "info" => InfoTab { gid: self.id.clone() }.into_element(),
            app => shown
                .iter()
                .find(|t| t.id == app)
                .map(|t| app_tab(t).into_element())
                .unwrap_or_else(|| rect().into_element()),
        };

        rect()
            .expanded()
            .padding((p, p, Dim::SPACE3, p))
            .spacing(Dim::SPACE3)
            .content(Content::flex())
            .child(
                rect()
                    .horizontal()
                    .width(Size::fill())
                    .content(Content::flex())
                    .spacing(Dim::SPACE2)
                    .cross_align(Alignment::Center)
                    .maybe(d.touch, |el| {
                        el.child(IconButton::new(Icon::ArrowLeft, move |_| nav.close_group()).tint(c.text_strong))
                    })
                    .child(
                        rect()
                            .width(Size::flex(1.))
                            .spacing(2.)
                            .child(txt(group_title(info), d.font_xl(), c.text_strong, 700).elide())
                            .maybe(info.alias.is_some(), |el| {
                                el.child(txt(info.haiku.clone(), d.font_sm(), c.text_muted, 400).elide())
                            }),
                    )
                    .child(
                        IconButton::new(Icon::Settings, move |_| {
                            nav.set_tab(if asked == "info" { &first } else { "info" })
                        })
                        .tint(if tab == "info" { c.accent_text } else { c.text_muted }),
                    ),
            )
            .maybe(!shown.is_empty(), |el| {
                el.child(TabStrip::new(
                    shown.iter().map(|t| (t.id.clone(), t.title.clone())).collect(),
                    tab.clone(),
                    move |t: String| nav.set_tab(&t),
                ))
            })
            .child(rect().width(Size::fill()).height(Size::flex(1.)).child(content))
            .into_element()
    }
}

// ---- files -----------------------------------------------------------------------------------

#[derive(PartialEq)]
struct FilesTab {
    gid: String,
    files: Vec<FileView>,
}

impl Component for FilesTab {
    fn render(&self) -> impl IntoElement {
        let (_, d) = look();
        let nav = use_consume::<Nav>();
        // Everything still offered, but only the latest few that no longer are, unless asked for all of them.
        let mut all = use_state(|| false);
        let hidden: Vec<&str> = if *all.read() {
            vec![]
        } else {
            self.files.iter().filter(|f| f.stopped).skip(PAST_FILES).map(|f| f.id.as_str()).collect()
        };
        let shown: Vec<&FileView> = self.files.iter().filter(|f| !hidden.contains(&f.id.as_str())).collect();
        let more = self.files.len() - shown.len();
        ScrollView::new()
            .expanded()
            .spacing(Dim::SPACE3)
            .child(
                card().child(
                    card_body(Dim::SPACE3).child(
                        rect()
                            .horizontal()
                            .width(Size::fill())
                            .content(Content::flex())
                            .spacing(Dim::SPACE2)
                            .child(Btn::primary("File").icon(Icon::File).expanded().on_press(move |_| nav.pick("file")))
                            .child(Btn::secondary("Media").icon(Icon::Image).expanded().on_press(move |_| nav.pick("media")))
                            .child(if d.touch {
                                Btn::secondary("App").icon(Icon::Apps).expanded().on_press(move |_| nav.pick("app"))
                            } else {
                                Btn::secondary("Folder").icon(Icon::Folder).expanded().on_press(move |_| nav.pick("folder"))
                            }),
                    ),
                ),
            )
            .children(shown.into_iter().map(|f| {
                FileCard { gid: self.gid.clone(), file: f.clone(), key: DiffKey::from(&f.id) }.into_element()
            }))
            .maybe(more > 0, |el| {
                el.child(Btn::ghost(format!("Show {more} more")).icon(Icon::ChevronDown).on_press(move |_| all.set(true)))
            })
            .maybe(self.files.is_empty(), |el| el.child(hint_center("Nothing offered yet.")))
    }
}

#[derive(PartialEq)]
struct FileCard {
    gid: String,
    file: FileView,
    key: DiffKey,
}

impl Component for FileCard {
    fn render(&self) -> impl IntoElement {
        let (c, d) = look();
        let nav = use_consume::<Nav>();
        let f = &self.file;
        let backend = nav.backend();
        let downloading = f.progress.is_some() && !f.preparing;
        let can_download = !f.mine && !f.have && !f.stopped && !downloading && f.source_online;
        let (done, total) = f.progress.unwrap_or((0, f.size));
        let progress = if total == 0 { 0. } else { done as f32 / total as f32 };
        let progress_label = if f.preparing {
            format!("Preparing: {} of {}", format::size(done), format::size(total))
        } else {
            format!("{} of {}", format::size(done), format::size(total))
        };
        let saved_to = f.saved_to.as_ref().map(|p| p.display().to_string());
        let can_host = !f.mine && f.have && !f.stopped && !f.hosting && saved_to.is_some();
        let hosting = f.hosting && !f.stopped;
        // A folder's size is only known once it has been gone through.
        let size = if f.preparing && f.size == 0 { "…".to_owned() } else { format::size(f.size) };
        let meta = format!(
            "{size} · {} · {}",
            if f.mine { "you".to_owned() } else { f.from_name.clone() },
            format::time(f.ts)
        );
        let tile = match f.kind.as_str() {
            "media" => Icon::Image,
            "apk" => Icon::Apps,
            "folder" => Icon::Folder,
            _ => Icon::File,
        };
        let status = match () {
            _ if f.preparing => Some(("Preparing", Tone::Active)),
            _ if f.stopped => Some(("Not shared anymore", Tone::Muted)),
            _ if f.mine => Some(("Sharing", Tone::Active)),
            _ if hosting => Some(("Hosting", Tone::Active)),
            _ if f.have => Some(("Downloaded", Tone::Ok)),
            _ if downloading => Some(("Downloading", Tone::Active)),
            _ if f.on_hold => Some(("On hold", Tone::Muted)),
            _ if !can_download => Some(("Sender offline", Tone::Warn)),
            _ => None,
        };
        let audience = (f.audience > 0).then(|| {
            let tone = match f.downloaded_by.len() {
                0 => Tone::Error,
                n if n >= f.audience => Tone::Ok,
                _ => Tone::Warn,
            };
            (format!("{} of {} got it", f.downloaded_by.len(), f.audience), tone)
        });

        let act = |f: fn(&flipflop::core::Core, &str, &str) -> anyhow::Result<()>| {
            let (backend, gid, id) = (backend.clone(), self.gid.clone(), self.file.id.clone());
            move |_| backend.run(|core| f(core, &gid, &id))
        };
        let (gid, id) = (self.gid.clone(), f.id.clone());

        // Taking a file off the list: from a right-click menu on the desktop, a button on phones.
        let removable = f.can_remove;
        let remove = act(|core, gid, id| core.remove_file(gid, id));
        let menu_remove = remove.clone();

        card()
            .maybe(removable && !d.touch, |el| {
                el.on_secondary_down(move |_| {
                    let remove = menu_remove.clone();
                    ContextMenu::open_from_down(
                        Menu::new().child(
                            MenuButton::new()
                                .on_press(move |_| {
                                    remove(());
                                    ContextMenu::close();
                                })
                                .child("Remove from the list"),
                        ),
                    );
                })
            })
            .child(
                card_body(Dim::SPACE2)
                    .child(
                        rect()
                            .horizontal()
                            .width(Size::fill())
                            .content(Content::flex())
                            .spacing(Dim::SPACE3)
                            .child(icon_tile(
                                tile,
                                40.,
                                if f.stopped { c.chip_bg } else { c.accent_soft },
                                if f.stopped { c.text_faint } else { c.accent_text },
                            ))
                            .child(
                                rect()
                                    .width(Size::flex(1.))
                                    .spacing(2.)
                                    .child(txt(f.name.clone(), d.font_md(), if f.stopped { c.text_muted } else { c.text_strong }, 650))
                                    .child(txt(meta, d.font_sm(), c.text_muted, 400))
                                    .maybe(!f.hosts.is_empty() && !f.stopped, |el| {
                                        el.child(txt(format!("Also from {}", f.hosts.join(", ")), d.font_sm(), c.text_muted, 400))
                                    }),
                            )
                            .maybe(removable && d.touch, |el| {
                                el.child(IconButton::new(Icon::Trash, remove).danger())
                            }),
                    )
                    .maybe(downloading || f.preparing, |el| {
                        el.child(
                            rect()
                                .width(Size::fill())
                                .spacing(4.)
                                .child(progress_bar(progress))
                                .child(txt(progress_label, d.font_xs(), c.text_faint, 400)),
                        )
                    })
                    .children(f.sending.iter().map(|s| {
                        let p = if s.total == 0 { 0. } else { s.done as f32 / s.total as f32 };
                        rect()
                            .width(Size::fill())
                            .spacing(4.)
                            .child(progress_bar(p))
                            .child(txt(
                                format!("Sending to {}: {} of {}", s.to, format::size(s.done), format::size(s.total)),
                                d.font_xs(),
                                c.text_faint,
                                400,
                            ))
                            .into_element()
                    }))
                    .map(f.error.clone().filter(|_| !downloading), |el, err| {
                        el.child(txt(err, d.font_sm(), c.error, 400))
                    })
                    .map(saved_to.clone().filter(|_| !f.mine), |el, path| {
                        el.child(txt(format!("Saved to {path}"), d.font_xs(), c.text_faint, 400))
                    })
                    .map(f.shared_from.as_ref(), |el, path| {
                        el.child(txt(format!("Shared from {}", path.display()), d.font_xs(), c.text_faint, 400))
                    })
                    .child(
                        rect()
                            .horizontal()
                            .width(Size::fill())
                            .content(Content::flex())
                            .spacing(Dim::SPACE2)
                            .cross_align(Alignment::Center)
                            .child(
                                rect()
                                    .horizontal()
                                    .width(Size::flex(1.))
                                    .spacing(Dim::SPACE2)
                                    .map(status, |el, (s, tone)| el.child(badge(s, tone)))
                                    .map(audience, |el, (s, tone)| el.child(badge(s, tone))),
                            )
                            .maybe(f.can_hold, |el| {
                                el.child(
                                    Btn::secondary(if downloading { "Cancel" } else { "Hold" })
                                        .on_press(act(|core, gid, id| core.hold_file(gid, id))),
                                )
                            })
                            .maybe(can_download, |el| {
                                let (backend, gid, id) = (backend.clone(), gid.clone(), id.clone());
                                el.child(
                                    Btn::primary(if f.error.is_some() { "Retry" } else { "Download" })
                                        .icon(Icon::ArrowDown)
                                        .on_press(move |_| backend.download_file(gid.clone(), id.clone())),
                                )
                            })
                            .map(saved_to, |el, path| {
                                el.child(
                                    Btn::secondary("Open")
                                        .icon(Icon::Open)
                                        .on_press(move |_| flipflop::platform::open_file(&path)),
                                )
                            })
                            .maybe(can_host, |el| {
                                el.child(
                                    Btn::secondary("Host it too")
                                        .icon(Icon::Share)
                                        .on_press(act(|core, gid, id| core.host_file(gid, id))),
                                )
                            })
                            .maybe(hosting, |el| {
                                el.child(Btn::secondary("Stop hosting").on_press(act(|core, gid, id| core.unhost_file(gid, id))))
                            })
                            .maybe(f.mine && !f.stopped && !f.preparing, |el| {
                                el.child(Btn::danger("Stop sharing").on_press(act(|core, gid, id| core.stop_file(gid, id))))
                            })
                            .maybe(f.can_reoffer, |el| {
                                let (backend, gid, id) = (backend.clone(), gid.clone(), id.clone());
                                el.child(
                                    Btn::secondary("Share again")
                                        .icon(Icon::Share)
                                        .on_press(move |_| backend.reoffer_file(gid.clone(), id.clone())),
                                )
                            }),
                    ),
            )
    }

    fn render_key(&self) -> DiffKey {
        self.key.clone().or(self.default_key())
    }
}

// ---- texts -----------------------------------------------------------------------------------

#[derive(PartialEq)]
struct TextsTab {
    gid: String,
    texts: Vec<TextView>,
}

impl Component for TextsTab {
    fn render(&self) -> impl IntoElement {
        let nav = use_consume::<Nav>();
        let mut draft = use_state(String::new);
        let gid = self.gid.clone();
        let send = move |_| {
            let text = draft.peek().clone();
            if text.is_empty() {
                return;
            }
            nav.backend().run(|core| core.send_text(&gid, &text));
            draft.set(String::new());
        };
        ScrollView::new()
            .expanded()
            .spacing(Dim::SPACE3)
            .child(
                card().child(
                    card_body(Dim::SPACE2)
                        .child(
                            area(draft, "A link, a code, anything to paste on the other devices…", move |text| {
                                draft.set(text)
                            }),
                        )
                        .child(
                            actions()
                                .child(Btn::secondary("Paste").icon(Icon::Clipboard).on_press(move |_| {
                                    let pasted = flipflop::platform::paste_text();
                                    draft.write().push_str(&pasted);
                                }))
                                .child(
                                    Btn::primary("Send")
                                        .icon(Icon::Send)
                                        .enabled(!draft.read().is_empty())
                                        .on_press(send),
                                ),
                        ),
                ),
            )
            .children(self.texts.iter().map(|t| text_card(t, &self.gid, nav)))
            .maybe(self.texts.is_empty(), |el| el.child(hint_center("Nothing sent or received yet.")))
    }
}

fn text_card(t: &TextView, gid: &str, nav: Nav) -> Element {
    let (c, d) = look();
    let (gid, id, text) = (gid.to_owned(), t.id.clone(), t.text.clone());
    let (open, copied) = (text.clone(), text.clone());
    card()
        .key(&t.id)
        .child(
            card_body(Dim::SPACE2)
                // Everything here is plain text, so let it be selected (and copied) like one.
                .child(
                    rect()
                        .horizontal()
                        .width(Size::fill())
                        .content(Content::flex())
                        .spacing(Dim::SPACE2)
                        .cross_align(Alignment::Center)
                        .child(
                            SelectableText::new()
                                .span(Span::new(if t.mine { format!("{} (you)", t.name) } else { t.name.clone() }))
                                .font_size(d.font_sm())
                                .color(c.accent_text)
                                .font_weight(650)
                                .max_lines(1)
                                .width(Size::flex(1.)),
                        )
                        .child(
                            SelectableText::new()
                                .span(Span::new(format::time(t.ts)))
                                .font_size(d.font_xs())
                                .color(c.text_faint),
                        ),
                )
                .child(
                    SelectableText::new()
                        .span(Span::new(text))
                        .font_size(d.font_md())
                        .color(c.text_strong)
                        .width(Size::fill()),
                )
                .child(
                    actions()
                        .maybe(t.mine, |el| {
                            el.child(
                                IconButton::new(Icon::Trash, move |_| {
                                    nav.backend().run(|core| core.delete_text(&gid, &id))
                                })
                                .danger(),
                            )
                        })
                        .maybe(format::is_link(&t.text), |el| {
                            el.child(
                                Btn::secondary("Open")
                                    .icon(Icon::Open)
                                    .on_press(move |_| flipflop::platform::open_url(open.trim())),
                            )
                        })
                        .child(Btn::secondary("Copy").icon(Icon::Copy).on_press(move |_| copy(nav, &copied))),
                ),
        )
        .into_element()
}

// ---- scratchpad ------------------------------------------------------------------------------

fn pad_actions(text: String, nav: Nav, gid: String) -> Rect {
    actions()
        .child(Btn::secondary("Copy all").icon(Icon::Copy).on_press(move |_| copy(nav, &text)))
        .child(Btn::secondary("Paste").icon(Icon::Clipboard).on_press(move |_| {
            // Appended: the editor keeps its own cursor, and a paste there goes through the keyboard.
            let pasted = flipflop::platform::paste_text();
            if pasted.is_empty() {
                return;
            }
            let gid = gid.clone();
            nav.backend().run(move |core| {
                let mut text = core.scratch(&gid).unwrap_or_default();
                text.push_str(&pasted);
                core.set_scratch(&gid, &text);
                Ok(())
            });
        }))
}

// ---- apps ------------------------------------------------------------------------------------

/// An app's tab. Running apps and showing their interface comes next; for now, where its file is.
fn app_tab(t: &TabView) -> Rect {
    let (c, d) = look();
    let Some(app) = &t.app else { return rect() };
    let status = match (app.ready, app.progress, &app.error) {
        (true, ..) => Some(("ready".to_owned(), Tone::Ok)),
        (_, Some((done, total)), _) => Some((
            format!("fetching {}%", (done * 100).checked_div(total).unwrap_or(0)),
            Tone::Active,
        )),
        _ if !app.source_online => Some(("waiting for someone who has it".to_owned(), Tone::Warn)),
        (_, _, Some(err)) => Some((err.clone(), Tone::Error)),
        _ => Some(("fetching".to_owned(), Tone::Active)),
    };
    card().child(
        card_body(Dim::SPACE3)
            .padding(Dim::SPACE5)
            .cross_align(Alignment::Center)
            .child(icon_tile(Icon::Apps, 56., c.accent_soft, c.accent_text))
            .child(txt(t.title.clone(), d.font_lg(), c.text_strong, 700))
            .child(txt(app_detail(t), d.font_sm(), c.text_muted, 400))
            .map(status, |el, (s, tone)| el.child(badge(s, tone)))
            .child(
                txt(
                    "Every device in the group fetches this app and shares it with the others. Running it here, with its hypereffect.dev interface, is coming.",
                    d.font_md(),
                    c.text_muted,
                    400,
                )
                .width(Size::fill())
                .text_align(TextAlign::Center),
            ),
    )
}

/// An app's "120 KB · added by Bob".
fn app_detail(t: &TabView) -> String {
    t.app
        .as_ref()
        .map(|a| format!("{} · added by {}", format::size(a.size), a.from_name))
        .unwrap_or_default()
}

// ---- info ------------------------------------------------------------------------------------

#[derive(PartialEq)]
struct InfoTab {
    gid: String,
}

impl Component for InfoTab {
    fn render(&self) -> impl IntoElement {
        let nav = use_consume::<Nav>();
        let peers = use_radio::<Model, Channel>(Channel::Peers);
        let groups = use_radio::<Model, Channel>(Channel::Groups);
        let _ = peers.read();
        let mut confirm_leave = use_state(|| false);
        let m = groups.read();
        let Some(info) = m.group(&self.gid) else {
            return rect().into_element();
        };
        let view = m.views.get(&self.gid).cloned().unwrap_or_default();
        let peer = info.with.and_then(|w| m.peers.iter().find(|p| p.id == w));

        let mut items: Vec<Element> = vec![];
        match peer {
            Some(peer) => items.extend(peer_section(peer, &m, nav)),
            None => items.extend(members_section(info, &m, nav)),
        }
        items.extend(tabs_section(&view, &self.gid, nav));
        items.extend(device_section(info, nav));
        if info.with.is_none() {
            items.extend(guest_section(info, nav));
            items.extend(leave_section(info, nav, &mut confirm_leave));
        }
        ScrollView::new().expanded().spacing(Dim::SPACE3).children(items).into_element()
    }
}

fn peer_section(peer: &Peer, m: &Model, nav: Nav) -> Vec<Element> {
    let (c, d) = look();
    let badges = rect()
        .horizontal()
        .spacing(4.)
        .child(badge("paired", Tone::Ok))
        .child(if peer.connected { badge("connected", Tone::Ok) } else { badge("offline", Tone::Muted) })
        .maybe(peer.lan, |el| el.child(badge("LAN", Tone::Active)))
        .maybe(peer.ble, |el| el.child(badge("Bluetooth", Tone::Warn)));
    let id = peer.id;
    let edit = rename_peer(peer);
    let invitable: Vec<(String, String)> =
        m.invitable(id).iter().map(|g| (g.id.clone(), group_title(g))).collect();
    let choice = nav.ui.read().invite_choice.min(invitable.len().saturating_sub(1));
    let mut ui = nav.ui;
    vec![
        overline("PEER").into_element(),
        card()
            .child(
                card_body(Dim::SPACE3)
                    .child(
                        rect()
                            .width(Size::fill())
                            .spacing(3.)
                            .child(txt(peer_title(peer), d.font_lg(), c.text_strong, 650).elide())
                            .child(txt(peer_subtitle(peer), d.font_xs(), c.text_faint, 400).elide())
                            .child(badges),
                    )
                    .child(
                        rect()
                            .horizontal()
                            .spacing(Dim::SPACE2)
                            .child(Btn::secondary("Rename").icon(Icon::Edit).on_press(move |_| nav.edit(Some(edit.clone()))))
                            .child(Btn::ghost("Unpair").on_press(move |_| nav.backend().run(|core| Ok(core.unpair(id))))),
                    ),
            )
            .into_element(),
        overline("INVITE TO A GROUP").into_element(),
        card()
            .child(card_body(Dim::SPACE2).map(
                (!invitable.is_empty()).then_some(invitable.clone()),
                |el, groups| {
                    let (gid, selected) = groups[choice].clone();
                    let title = selected.clone();
                    el.child(
                        Select::new().selected_item(selected).children(groups.into_iter().enumerate().map(
                            |(i, (_, title))| {
                                MenuItem::new()
                                    .selected(i == choice)
                                    .on_press(move |_| ui.write().invite_choice = i)
                                    .child(title)
                                    .into_element()
                            },
                        )),
                    )
                    .child(Btn::primary("Invite").icon(Icon::AddPeer).on_press(move |_| {
                        nav.backend().run(|core| Ok(core.add_to_group(&gid, id)));
                        nav.toast(format!("Invited to {title}"));
                    }))
                },
            ))
            .maybe(invitable.is_empty(), |el| {
                el.child(card_body(0.).child(hint(
                    "No group to invite them to: create a group first, or they are in all of yours already.",
                )))
            })
            .into_element(),
    ]
}

fn members_section(info: &GroupInfo, m: &Model, nav: Nav) -> Vec<Element> {
    let (c, d) = look();
    let members = m.members(info);
    let candidates: Vec<_> = info
        .candidates
        .iter()
        .filter(|(id, _)| !members.iter().any(|row| row.id == id.to_string()))
        .cloned()
        .collect();
    let row_height = d.control_height() + 12.;
    vec![
        overline(if info.guest.is_some() { "GUESTS AND MEMBERS" } else { "MEMBERS" }).into_element(),
        card()
            .child(
                card_body(0.)
                    .children(members.iter().map(|row| {
                        rect()
                            .horizontal()
                            .width(Size::fill())
                            .content(Content::flex())
                            .height(Size::px(row_height))
                            .spacing(Dim::SPACE2)
                            .cross_align(Alignment::Center)
                            .child(dot(row.online, 8.))
                            .child(
                                txt(
                                    if row.me { format!("{} (you)", row.name) } else { row.name.clone() },
                                    d.font_md(),
                                    c.text_strong,
                                    400,
                                )
                                .width(Size::flex(1.))
                                .elide(),
                            )
                            .child(badge(row.status, row.tone))
                            .into_element()
                    }))
                    .maybe(members.is_empty(), |el| el.child(hint("Nobody yet.")))
                    .child(divider())
                    .child(txt("Invite", d.font_sm(), c.text_muted, 400).margin((Dim::SPACE2, 0., 0., 0.)))
                    .children(candidates.iter().map(|(id, name)| {
                        let (gid, id) = (info.id.clone(), *id);
                        rect()
                            .horizontal()
                            .width(Size::fill())
                            .content(Content::flex())
                            .height(Size::px(row_height))
                            .spacing(Dim::SPACE3)
                            .cross_align(Alignment::Center)
                            .child(txt(name.clone(), d.font_md(), c.text_strong, 400).width(Size::flex(1.)).elide())
                            .child(Btn::secondary("Invite").on_press(move |_| {
                                nav.backend().run(|core| Ok(core.add_to_group(&gid, id)))
                            }))
                            .into_element()
                    }))
                    .maybe(candidates.is_empty(), |el| {
                        el.child(hint("Every peer you know is already in, or has been invited."))
                    }),
            )
            .into_element(),
    ]
}

/// The group's tabs, the same for every member: anyone shows or hides them, or adds an app.
fn tabs_section(view: &GroupView, gid: &str, nav: Nav) -> Vec<Element> {
    let mut rows: Vec<Element> = vec![];
    for (i, t) in view.tabs.iter().enumerate() {
        if i > 0 {
            rows.push(divider().into_element());
        }
        let hint = match t.id.as_str() {
            "files" => "Files offered to the group.".to_owned(),
            "texts" => "Links, codes and snippets sent to the group.".to_owned(),
            "pad" => "A text everyone edits together.".to_owned(),
            _ => app_detail(t),
        };
        let (gid, tab) = (gid.to_owned(), t.id.clone());
        rows.push(
            ToggleRow::new(t.title.clone(), hint, t.on, move |on| {
                nav.backend().run(|core| core.set_tab(&gid, &tab, on))
            })
            .into_element(),
        );
    }
    rows.push(divider().into_element());
    rows.push(Btn::secondary("Add an app").icon(Icon::Apps).on_press(move |_| nav.pick("wasm")).into_element());
    vec![
        overline("TABS").into_element(),
        card().child(card_body(Dim::SPACE2).children(rows)).into_element(),
        hint("Tabs are the same for everyone in the group, and anyone can show or hide them. An app is a WebAssembly file: every device fetches it and shares it with the others.").into_element(),
    ]
}

fn device_section(info: &GroupInfo, nav: Nav) -> Vec<Element> {
    let setting = |which: u8| {
        let gid = info.id.clone();
        move |on: bool| {
            nav.backend().run(|core| {
                match which {
                    0 => core.set_group_settings(&gid, Some(on), None, None),
                    1 => core.set_group_settings(&gid, None, Some(on), None),
                    _ => core.set_group_settings(&gid, None, None, Some(on)),
                }
                Ok(())
            })
        }
    };
    vec![
        overline("ON THIS DEVICE").into_element(),
        card()
            .child(
                card_body(Dim::SPACE2)
                    .child(ToggleRow::new(
                        "Download every file automatically",
                        "New offers from the group are fetched as soon as the sender is reachable.",
                        info.auto_download,
                        setting(0),
                    ))
                    .child(divider())
                    .child(ToggleRow::new(
                        "Stop sharing a file once everybody has it",
                        "Applies to the files you offer. You can always stop one by hand.",
                        info.auto_stop,
                        setting(1),
                    ))
                    .child(divider())
                    .child(ToggleRow::new(
                        "Automatically rehost received files",
                        "Serve the files you download to the rest of the group too, so they stay available when the sender is offline.",
                        info.auto_rehost,
                        setting(2),
                    )),
            )
            .into_element(),
    ]
}

/// One address of a guest link: its QR code, the text and a Copy button.
fn guest_address(title: &str, hint_text: &str, url: String, nav: Nav) -> Element {
    let (c, d) = look();
    let width = Platform::get().current_window().root_size.read().width as f32;
    let size = (width - 2. * d.page_padding() - 2. * d.card_padding()).clamp(120., 220.);
    card()
        .child(
            card_body(Dim::SPACE2)
                .child(txt(title, d.font_md(), c.text_strong, 650))
                .child(hint(hint_text))
                .child(rect().width(Size::fill()).cross_align(Alignment::Center).child(qr_code(&url, size)))
                .child(copyable(url.clone()))
                .child(Btn::secondary("Copy link").icon(Icon::Copy).on_press(move |_| copy(nav, &url))),
        )
        .into_element()
}

fn guest_section(info: &GroupInfo, nav: Nav) -> Vec<Element> {
    let guest = info.guest.clone();
    let gid = info.id.clone();
    let uploads_gid = gid.clone();
    let mut items = vec![
        overline("WEB GUESTS").into_element(),
        card()
            .child(
                card_body(Dim::SPACE2)
                    .child(ToggleRow::new(
                        "Allow guests from a browser",
                        "Anyone with the secret link joins this group from a web page, no app needed.",
                        guest.is_some(),
                        move |on| {
                            let gid = gid.clone();
                            nav.backend().run(move |core| {
                                if on {
                                    core.enable_guest_link(&gid, false);
                                } else {
                                    core.disable_guest_link(&gid);
                                }
                                Ok(())
                            })
                        },
                    ))
                    .map(guest.as_ref().map(|g| g.uploads), |el, uploads| {
                        el.child(divider()).child(ToggleRow::new(
                            "Guests can upload files",
                            "Even when off they can still download files, write texts and edit the scratchpad.",
                            uploads,
                            move |on| {
                                let gid = uploads_gid.clone();
                                nav.backend().run(move |core| Ok(core.set_guest_uploads(&gid, on)))
                            },
                        ))
                    }),
            )
            .into_element(),
    ];
    if let Some(g) = guest {
        items.push(match g.lan_url {
            Some(url) => guest_address(
                "Same network",
                "Opens a page served by this device. Works without internet, as long as the guest is on the same Wi-Fi.",
                url,
                nav,
            ),
            None => unavailable("Same network", "Not available because you're not connected to any Wi-Fi networks."),
        });
        items.push(match g.web_url {
            Some(url) => guest_address(
                "Anywhere",
                "Opens the FlipFlop page on the web, uses Iroh to connect to this device.",
                url,
                nav,
            ),
            None => unavailable("Anywhere", "Not available because you're not connected to the internet."),
        });
        items.push(hint("Anyone with a link joins the group and can do things in it. Only share it with people you trust.").into_element());
    }
    items
}

fn unavailable(title: &str, why: &str) -> Element {
    let (c, d) = look();
    card()
        .child(card_body(Dim::SPACE2).child(txt(title, d.font_md(), c.text_strong, 650)).child(hint(why)))
        .into_element()
}

fn leave_section(info: &GroupInfo, nav: Nav, confirm: &mut State<bool>) -> Vec<Element> {
    let mut confirm = *confirm;
    let guest = info.guest.is_some();
    let gid = info.id.clone();
    let edit = Edit {
        kind: EditKind::Group,
        id: info.id.clone(),
        original: info.haiku.clone(),
        text: info.alias.clone().unwrap_or_default(),
    };
    let closing = if guest { "Close link" } else { "Leave group" };
    vec![
        rect()
            .horizontal()
            .spacing(Dim::SPACE2)
            .child(Btn::secondary("Rename").icon(Icon::Edit).on_press(move |_| nav.edit(Some(edit.clone()))))
            // Leaving needs a second press.
            .child(
                Btn::danger(if *confirm.read() { "Confirm" } else { closing })
                    .icon(Icon::Trash)
                    .on_press(move |_| {
                        if !*confirm.peek() {
                            confirm.set(true);
                            return;
                        }
                        confirm.set(false);
                        let gid = gid.clone();
                        if guest {
                            nav.backend().run(move |core| Ok(core.disable_guest_link(&gid)));
                        } else {
                            nav.close_group();
                            nav.backend().run(move |core| Ok(core.delete_group(&gid)));
                        }
                    }),
            )
            .into_element(),
        hint(if guest {
            "Closing the link disconnects its guests and keeps the group, with its members and files."
        } else {
            "Leaving removes the group from this device only. Downloaded files are not affected."
        })
        .into_element(),
    ]
}
