//! The music tab: what the group offers to play, and this device's queue. The queue and the player
//! live in the backend ([`crate::player`]), so playing goes on without the UI; here they are shown
//! from the [`MusicState`] it reports.

use flipflop::{core::FileView, format};
use freya::prelude::*;

use crate::{
    icons::Icon,
    nav::Nav,
    player::MusicState,
    theme::{Dim, look},
    widgets::*,
};

/// Queue rows shown before the queue scrolls.
const QUEUE_ROWS: usize = 4;

/// A queue row being dragged: its song's key.
#[derive(Clone, Copy, PartialEq)]
struct Dragged(u64);

/// What plays, as the backend last said; provided at the root.
pub fn use_init_music() -> State<MusicState> {
    use_provide_context(|| State::create(MusicState::default()))
}

/// Do something with the backend's music.
fn with_music(nav: Nav, f: impl FnOnce(&crate::player::Music, &crate::backend::Backend)) {
    let backend = nav.backend();
    f(backend.music(), &backend);
}

// ---- the tab ---------------------------------------------------------------------------------

#[derive(PartialEq)]
pub struct MusicTab {
    pub gid: String,
    pub music: Vec<FileView>,
}

impl Component for MusicTab {
    fn render(&self) -> impl IntoElement {
        let nav = use_consume::<Nav>();
        let state = use_consume::<State<MusicState>>();
        let active = {
            let now = state.read();
            now.current.is_some() || !now.queue.is_empty()
        };
        let list = ScrollView::new()
            .width(Size::fill())
            .height(Size::flex(1.))
            .spacing(Dim::SPACE3)
            .child(card().child(
                card_body(Dim::SPACE3).child(
                    rect()
                        .horizontal()
                        .width(Size::fill())
                        .content(Content::flex())
                        .spacing(Dim::SPACE2)
                        .child(Btn::primary("Add songs").icon(Icon::Music).expanded().on_press(move |_| nav.pick("music")))
                        // Albums are folders, which only the desktop's picker gives.
                        .maybe(cfg!(not(target_os = "android")), |el| {
                            el.child(
                                Btn::secondary("Add a folder")
                                    .icon(Icon::Folder)
                                    .expanded()
                                    .on_press(move |_| nav.pick("music-folder")),
                            )
                        }),
                ),
            ))
            .children(self.music.iter().map(|f| OfferCard { gid: self.gid.clone(), offer: f.clone() }.into_element()))
            .maybe(self.music.is_empty(), |el| {
                el.child(hint_center("Nothing to play yet. Offer songs: the others play them from you, nobody downloads them."))
            });
        rect()
            .expanded()
            .spacing(Dim::SPACE3)
            .content(Content::flex())
            .child(list)
            .maybe(active, |el| el.child(QueuePanel))
    }
}

/// One offer: a song, or an album that unfolds into its songs.
#[derive(PartialEq)]
struct OfferCard {
    gid: String,
    offer: FileView,
}

impl Component for OfferCard {
    fn render(&self) -> impl IntoElement {
        let (c, d) = look();
        let nav = use_consume::<Nav>();
        let now = use_consume::<State<MusicState>>().read().clone();
        let mut open = use_state(|| false);
        let (gid, f) = (&self.gid, &self.offer);
        let album = !f.tracks.is_empty();
        let current = now.current.as_ref().filter(|s| s.of(gid, &f.id));
        let playing = current.is_some() && !now.paused;
        let who = if f.mine { "you".to_owned() } else { f.from_name.clone() };
        let meta = if album {
            let n = f.tracks.len();
            format!("{n} song{} · {} · {who} · {}", if n == 1 { "" } else { "s" }, format::size(f.size), format::time(f.ts))
        } else {
            format!("{} · {who} · {}", format::size(f.size), format::time(f.ts))
        };
        let (press, queue) = ((gid.clone(), f.clone()), (gid.clone(), f.clone()));
        let (stop_gid, stop_id) = (gid.clone(), f.id.clone());
        let can_play = !f.preparing && (f.mine || f.source_online);
        let ink = if current.is_some() { c.accent_text } else { c.text_strong };
        let head = rect()
            .horizontal()
            .width(Size::fill())
            .content(Content::flex())
            .spacing(Dim::SPACE3)
            .cross_align(Alignment::Center)
            .child(icon_tile(if album { Icon::Folder } else { Icon::Music }, 40., c.accent_soft, c.accent_text))
            .child(
                rect()
                    .width(Size::flex(1.))
                    .spacing(2.)
                    .maybe(album, |el| el.cursor(CursorIcon::Pointer).on_press(move |_| open.toggle()))
                    .child(txt(f.name.clone(), d.font_md(), ink, 650).elide())
                    .child(txt(meta, d.font_sm(), c.text_muted, 400).elide())
                    .maybe(f.preparing, |el| el.child(txt("Preparing…", d.font_xs(), c.text_faint, 400)))
                    .maybe(!f.preparing && !can_play, |el| el.child(txt("Sender offline", d.font_xs(), c.warn, 400))),
            )
            .maybe(can_play, |el| {
                el.child(
                    IconButton::new(if playing { Icon::Pause } else { Icon::Play }, move |_| {
                        with_music(nav, |m, b| {
                            if album {
                                m.press_album(b, &press.0, &press.1)
                            } else {
                                m.press(b, &press.0, &press.1, 0)
                            }
                        })
                    })
                    .tint(c.accent_text),
                )
                .child(IconButton::new(Icon::Plus, move |_| {
                    with_music(nav, |m, b| m.enqueue(b, m.songs(&queue.0, &queue.1)));
                    nav.toast(format!("Queued {}", queue.1.name));
                }))
            })
            .maybe(album, |el| {
                el.child(IconButton::new(if *open.read() { Icon::ChevronDown } else { Icon::ChevronRight }, move |_| {
                    open.toggle()
                }))
            })
            .maybe(f.mine && !f.preparing, |el| {
                el.child(
                    IconButton::new(Icon::Trash, move |_| nav.backend().run(|core| core.stop_file(&stop_gid, &stop_id)))
                        .danger(),
                )
            });
        let tracks = f.tracks.iter().enumerate().map(|(i, path)| {
            let this = now.current.as_ref().is_some_and(|s| s.is(gid, &f.id, i));
            let (press, queue) = ((gid.clone(), f.clone()), (gid.clone(), f.clone()));
            let name = path.rsplit('/').next().unwrap_or(path).to_owned();
            let row_name = name.clone();
            rect()
                .key(i)
                .horizontal()
                .width(Size::fill())
                .content(Content::flex())
                .spacing(Dim::SPACE2)
                .cross_align(Alignment::Center)
                .padding((0., 0., 0., 52.))
                .child(
                    txt(format!("{}. {row_name}", i + 1), d.font_sm(), if this { c.accent_text } else { c.text_strong }, 400)
                        .width(Size::flex(1.))
                        .elide(),
                )
                .maybe(can_play, |el| {
                    el.child(
                        IconButton::new(if this && !now.paused { Icon::Pause } else { Icon::Play }, move |_| {
                            with_music(nav, |m, b| m.press(b, &press.0, &press.1, i))
                        })
                        .tint(c.accent_text)
                        .size(d.control_height() - 8.),
                    )
                    .child(
                        IconButton::new(Icon::Plus, move |_| {
                            with_music(nav, |m, b| {
                                let songs = m.songs(&queue.0, &queue.1);
                                m.enqueue(b, songs.into_iter().skip(i).take(1).collect())
                            });
                            nav.toast(format!("Queued {name}"));
                        })
                        .size(d.control_height() - 8.),
                    )
                })
                .into_element()
        });
        card().child(
            card_body(Dim::SPACE2)
                .child(head)
                .maybe(album && *open.read(), |el| el.child(divider()).children(tracks)),
        )
    }

    fn render_key(&self) -> DiffKey {
        DiffKey::from(&self.offer.id)
    }
}

/// What is playing, and what plays next, only while there is either. Folded by default; unfolded,
/// the rows (dragged to reorder, each with a button to take it out) scroll past [`QUEUE_ROWS`].
#[derive(PartialEq)]
struct QueuePanel;

impl Component for QueuePanel {
    fn render(&self) -> impl IntoElement {
        let (c, d) = look();
        let nav = use_consume::<Nav>();
        let state = use_consume::<State<MusicState>>();
        let mut over = use_state(|| None::<Option<u64>>);
        let mut open = use_state(|| false);
        let unfolded = *open.read();
        let MusicState { queue, current, paused, loading } = state.read().clone();
        let row_height = d.control_height() + 8.;

        let (title, mut detail) = match &current {
            Some(e) if loading => (e.name.clone(), format!("Starting… · {}", e.from_name)),
            Some(e) => (e.name.clone(), e.from_name.clone()),
            None => ("Nothing playing".to_owned(), "Press play to start the queue".to_owned()),
        };
        if !queue.is_empty() {
            detail = format!("{detail} · {} up next", queue.len());
        }
        let now = rect()
            .horizontal()
            .width(Size::fill())
            .content(Content::flex())
            .spacing(Dim::SPACE2)
            .cross_align(Alignment::Center)
            .child(
                rect()
                    .horizontal()
                    .width(Size::flex(1.))
                    .content(Content::flex())
                    .spacing(Dim::SPACE2)
                    .cross_align(Alignment::Center)
                    .cursor(CursorIcon::Pointer)
                    .on_press(move |_| open.toggle())
                    .child(icon(if unfolded { Icon::ChevronDown } else { Icon::ChevronUp }, 18., c.text_faint))
                    .child(
                        rect()
                            .width(Size::flex(1.))
                            .spacing(2.)
                            .child(txt(title, d.font_md(), c.text_strong, 650).elide())
                            .child(txt(detail, d.font_sm(), c.text_muted, 400).elide()),
                    ),
            )
            .child(
                IconButton::new(if current.is_some() && !paused { Icon::Pause } else { Icon::Play }, move |_| {
                    with_music(nav, |m, b| m.toggle(b))
                })
                .tint(c.accent_text),
            )
            .maybe(current.is_some(), |el| el.child(IconButton::new(Icon::Skip, move |_| with_music(nav, |m, b| m.next(b)))));

        let rows = queue.iter().map(|e| {
            let key = e.key;
            let highlighted = *over.read() == Some(Some(key));
            let row = rect()
                .horizontal()
                .width(Size::fill())
                .height(Size::px(row_height))
                .content(Content::flex())
                .spacing(Dim::SPACE2)
                .cross_align(Alignment::Center)
                .padding((0., Dim::SPACE2))
                .corner_radius(8.)
                .background(if highlighted { c.chip_bg } else { Color::TRANSPARENT })
                .child(icon(Icon::Grip, 16., c.text_faint))
                .child(txt(e.name.clone(), d.font_md(), c.text_strong, 400).width(Size::flex(1.)).elide())
                .child(txt(e.from_name.clone(), d.font_sm(), c.text_muted, 400).max_lines(1))
                .child(
                    IconButton::new(Icon::Close, move |_| with_music(nav, |m, b| m.remove(b, key))).size(row_height - 8.),
                );
            DropZone::new(move |Dragged(dragged)| {
                over.set(None);
                with_music(nav, |m, b| m.reorder(b, dragged, Some(key)));
            })
            .on_drag_over(move |on: bool| over.set(on.then_some(Some(key))))
            .key(key)
            .width(Size::fill())
            .child(
                DragZone::new(Dragged(key))
                    .width(Size::fill())
                    .drag_element(
                        card().width(Size::px(260.)).child(
                            card_body(0.)
                                .padding((Dim::SPACE2, Dim::SPACE3))
                                .child(txt(e.name.clone(), d.font_md(), c.text_strong, 600).elide()),
                        ),
                    )
                    .child(row),
            )
            .into_element()
        });
        // Dropped below the last row: to the end.
        let end = DropZone::new(move |Dragged(dragged)| {
            over.set(None);
            with_music(nav, |m, b| m.reorder(b, dragged, None));
        })
        .on_drag_over(move |on: bool| over.set(on.then_some(None)))
        .width(Size::fill())
        .child(
            rect()
                .width(Size::fill())
                .height(Size::px(Dim::SPACE3))
                .corner_radius(4.)
                .background(if *over.read() == Some(None) { c.chip_bg } else { Color::TRANSPARENT }),
        );

        let shown = queue.len().min(QUEUE_ROWS) as f32;
        card().child(
            card_body(Dim::SPACE2)
                .child(now)
                .maybe(unfolded, |el| {
                    el.child(divider())
                        .child(
                            rect()
                                .horizontal()
                                .width(Size::fill())
                                .content(Content::flex())
                                .cross_align(Alignment::Center)
                                .child(
                                    txt(format!("Up next · {}", queue.len()), d.font_xs(), c.text_faint, 600)
                                        .width(Size::flex(1.)),
                                )
                                .child(Btn::ghost("Clear").icon(Icon::Trash).on_press(move |_| with_music(nav, |m, b| m.clear(b)))),
                        )
                        .child(
                            ScrollView::new()
                                .width(Size::fill())
                                .height(Size::px(shown * row_height + Dim::SPACE3))
                                .children(rows)
                                .child(end),
                        )
                }),
        )
    }
}
