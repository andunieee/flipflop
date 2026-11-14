//! The music tab: what the group offers to play, and this device's queue. The queue and the player
//! live in the backend ([`crate::player`]), so playing goes on without the UI; here they are shown
//! from the [`MusicState`] it reports.

use std::{collections::HashSet, time::Duration};

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

/// How often the playback clock reads where the song is.
const CLOCK_TICK: Duration = Duration::from_millis(100);

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

/// What an offer's card shows of what plays: which of its songs is current, and whether it is
/// paused. Cards get only this, so playing re-renders only the cards it concerns.
#[derive(Clone, Copy, PartialEq)]
struct Playing {
    track: usize,
    paused: bool,
}

/// What the visible rows are built from; the list rebuilds them when it changes.
#[derive(Clone, PartialEq)]
struct Rows {
    gid: String,
    music: Vec<FileView>,
    current: Option<(String, Playing)>,
    open: HashSet<String>,
}

/// Height of an offer's head: its icon, buttons, and two or (`status`) three lines of text.
fn head_height(d: Dim, status: bool) -> f32 {
    let text = (d.font_md() + d.font_sm() + if status { d.font_xs() } else { 0. }) * 1.35 + 4.;
    text.max(d.control_height()).max(40.)
}

fn track_height(d: Dim) -> f32 {
    d.control_height() - 8.
}

/// What an offer's row takes in the list, the gap below it included. The list only builds the rows
/// in view (a group can offer hundreds of songs), so it must know each row's height beforehand.
fn row_height(d: Dim, f: &FileView, open: bool) -> f32 {
    let status = f.preparing || !(f.mine || f.source_online);
    let mut card = head_height(d, status) + 2. * d.card_padding();
    if open && !f.tracks.is_empty() {
        card += Dim::SPACE2 + 1. + f.tracks.len() as f32 * (Dim::SPACE2 + track_height(d));
    }
    card + Dim::SPACE3
}

impl Component for MusicTab {
    fn render(&self) -> impl IntoElement {
        let (_, d) = look();
        let nav = use_consume::<Nav>();
        let state = use_consume::<State<MusicState>>();
        let open = use_state(HashSet::<String>::new);
        let (active, current) = {
            let now = state.read();
            let current = now.current.as_ref().filter(|s| s.gid == self.gid).map(|s| {
                (
                    s.fid.clone(),
                    Playing {
                        track: s.track,
                        paused: now.paused,
                    },
                )
            });
            (now.current.is_some() || !now.queue.is_empty(), current)
        };
        let add = card().child(
            card_body(Dim::SPACE3).child(
                rect()
                    .horizontal()
                    .width(Size::fill())
                    .content(Content::flex())
                    .spacing(Dim::SPACE2)
                    .child(
                        Btn::primary("Add songs")
                            .icon(Icon::Music)
                            .expanded()
                            .on_press(move |_| nav.pick("music")),
                    )
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
        );
        if self.music.is_empty() {
            return rect()
                .expanded()
                .spacing(Dim::SPACE3)
                .content(Content::flex())
                .child(add)
                .child(hint_center("Nothing to play yet. Offer songs: the others play them from you, nobody downloads them."))
                .maybe(active, |el| el.child(QueuePanel))
                .into_element();
        }
        let along = Listeners {
            gid: self.gid.clone(),
        };
        let rows = Rows {
            gid: self.gid.clone(),
            music: self.music.clone(),
            current,
            open: open.read().clone(),
        };
        // The add buttons' card first, then the offers.
        let sizes: Vec<f32> =
            std::iter::once(d.control_height() + 2. * d.card_padding() + Dim::SPACE3)
                .chain(
                    rows.music
                        .iter()
                        .map(|f| row_height(d, f, rows.open.contains(&f.id))),
                )
                .collect();
        let length = sizes.len();
        let list = VirtualScrollView::new_with_data(rows, move |item, rows: &Rows| {
            let Some(f) = item.index.checked_sub(1).and_then(|i| rows.music.get(i)) else {
                return rect()
                    .key(0)
                    .height(Size::px(item.size))
                    .child(add.clone())
                    .into();
            };
            let playing = rows
                .current
                .as_ref()
                .filter(|(fid, _)| *fid == f.id)
                .map(|(_, p)| *p);
            rect()
                .key(&f.id)
                .height(Size::px(item.size))
                .child(OfferCard {
                    gid: rows.gid.clone(),
                    offer: f.clone(),
                    playing,
                    open: rows.open.contains(&f.id),
                    opened: open,
                })
                .into()
        })
        .item_size(move |i: usize| sizes[i])
        .length(length)
        .width(Size::fill())
        .height(Size::flex(1.));
        rect()
            .expanded()
            .spacing(Dim::SPACE3)
            .content(Content::flex())
            .child(along)
            .child(list)
            .maybe(active, |el| el.child(QueuePanel))
            .into_element()
    }
}

/// "Ann, Bob and Cy".
fn and_list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The others sharing what they play in the group, each with a button to listen along.
#[derive(PartialEq)]
struct Listeners {
    gid: String,
}

impl Component for Listeners {
    fn render(&self) -> impl IntoElement {
        let (c, d) = look();
        let nav = use_consume::<Nav>();
        let state = use_consume::<State<MusicState>>();
        let (sharers, following) = {
            let now = state.read();
            (
                now.sharers.get(&self.gid).cloned().unwrap_or_default(),
                now.following.clone(),
            )
        };
        rect()
            .width(Size::fill())
            .spacing(Dim::SPACE2)
            .children(sharers.into_iter().map(|v| {
                let along = following
                    .as_ref()
                    .is_some_and(|f| f.gid == self.gid && f.sharer == v.sharer);
                let (gid, sharer) = (self.gid.clone(), v.sharer);
                let what = if v.now.paused {
                    format!("{} · paused", v.now.name)
                } else {
                    v.now.name.clone()
                };
                card()
                    .key(v.sharer.to_string())
                    .child(
                        card_body(0.).child(
                            rect()
                                .horizontal()
                                .width(Size::fill())
                                .content(Content::flex())
                                .spacing(Dim::SPACE3)
                                .cross_align(Alignment::Center)
                                .child(icon_tile(
                                    Icon::Headphones,
                                    40.,
                                    c.accent_soft,
                                    c.accent_text,
                                ))
                                .child(
                                    rect()
                                        .width(Size::flex(1.))
                                        .spacing(2.)
                                        .child(
                                            txt(
                                                format!("{} is listening", v.name),
                                                d.font_md(),
                                                c.text_strong,
                                                650,
                                            )
                                            .elide(),
                                        )
                                        .child(txt(what, d.font_sm(), c.text_muted, 400).elide()),
                                )
                                .child(if along {
                                    Btn::secondary("Leave")
                                        .icon(Icon::Close)
                                        .on_press(move |_| {
                                            with_music(nav, |m, b| m.stop_following(b))
                                        })
                                } else {
                                    Btn::primary("Listen along")
                                        .icon(Icon::Headphones)
                                        .on_press(move |_| {
                                            with_music(nav, |m, b| m.listen_along(b, &gid, sharer))
                                        })
                                }),
                        ),
                    )
                    .into_element()
            }))
    }
}

/// Unfold the album `id`, or fold it.
fn toggle(mut opened: State<HashSet<String>>, id: &str) {
    let mut set = opened.write();
    if !set.remove(id) {
        set.insert(id.to_owned());
    }
}

/// One offer: a song, or an album that unfolds into its songs.
#[derive(PartialEq)]
struct OfferCard {
    gid: String,
    offer: FileView,
    playing: Option<Playing>,
    open: bool,
    /// The albums unfolded, kept by the tab: its rows are built only while in view.
    opened: State<HashSet<String>>,
}

impl Component for OfferCard {
    fn render(&self) -> impl IntoElement {
        let (c, d) = look();
        let nav = use_consume::<Nav>();
        let (gid, f) = (&self.gid, &self.offer);
        let (open, opened) = (self.open, self.opened);
        let album = !f.tracks.is_empty();
        let current = self.playing;
        let playing = current.is_some_and(|p| !p.paused);
        let who = if f.mine {
            "you".to_owned()
        } else {
            f.from_name.clone()
        };
        let meta = if album {
            let n = f.tracks.len();
            format!(
                "{n} song{} · {} · {who} · {}",
                if n == 1 { "" } else { "s" },
                format::size(f.size),
                format::time(f.ts)
            )
        } else {
            format!("{} · {who} · {}", format::size(f.size), format::time(f.ts))
        };
        let (press, queue) = ((gid.clone(), f.clone()), (gid.clone(), f.clone()));
        let (stop_gid, stop_id) = (gid.clone(), f.id.clone());
        let can_play = !f.preparing && (f.mine || f.source_online);
        let ink = if current.is_some() {
            c.accent_text
        } else {
            c.text_strong
        };
        let head = rect()
            .horizontal()
            .width(Size::fill())
            .height(Size::px(head_height(d, f.preparing || !can_play)))
            .content(Content::flex())
            .spacing(Dim::SPACE3)
            .cross_align(Alignment::Center)
            .child(icon_tile(
                if album { Icon::Folder } else { Icon::Music },
                40.,
                c.accent_soft,
                c.accent_text,
            ))
            .child(
                rect()
                    .width(Size::flex(1.))
                    .spacing(2.)
                    .maybe(album, |el| {
                        el.cursor(CursorIcon::Pointer).on_press({
                            let id = f.id.clone();
                            move |_| toggle(opened, &id)
                        })
                    })
                    .child(txt(f.name.clone(), d.font_md(), ink, 650).elide())
                    .child(txt(meta, d.font_sm(), c.text_muted, 400).elide())
                    .maybe(f.preparing, |el| {
                        el.child(txt("Preparing…", d.font_xs(), c.text_faint, 400))
                    })
                    .maybe(!f.preparing && !can_play, |el| {
                        el.child(txt("Sender offline", d.font_xs(), c.warn, 400))
                    }),
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
                el.child(IconButton::new(
                    if open {
                        Icon::ChevronDown
                    } else {
                        Icon::ChevronRight
                    },
                    {
                        let id = f.id.clone();
                        move |_| toggle(opened, &id)
                    },
                ))
            })
            .maybe(f.mine && !f.preparing, |el| {
                el.child(
                    IconButton::new(Icon::Trash, move |_| {
                        nav.backend()
                            .run(|core| core.stop_file(&stop_gid, &stop_id))
                    })
                    .danger(),
                )
            });
        let tracks = f.tracks.iter().enumerate().map(|(i, path)| {
            let this = current.filter(|p| p.track == i);
            let (press, queue) = ((gid.clone(), f.clone()), (gid.clone(), f.clone()));
            let name = path.rsplit('/').next().unwrap_or(path).to_owned();
            let row_name = name.clone();
            rect()
                .key(i)
                .horizontal()
                .width(Size::fill())
                .height(Size::px(track_height(d)))
                .content(Content::flex())
                .spacing(Dim::SPACE2)
                .cross_align(Alignment::Center)
                .padding((0., 0., 0., 52.))
                .child(
                    txt(
                        format!("{}. {row_name}", i + 1),
                        d.font_sm(),
                        if this.is_some() {
                            c.accent_text
                        } else {
                            c.text_strong
                        },
                        400,
                    )
                    .width(Size::flex(1.))
                    .elide(),
                )
                .maybe(can_play, |el| {
                    el.child(
                        IconButton::new(
                            if this.is_some_and(|p| !p.paused) {
                                Icon::Pause
                            } else {
                                Icon::Play
                            },
                            move |_| with_music(nav, |m, b| m.press(b, &press.0, &press.1, i)),
                        )
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
                .maybe(album && open, |el| el.child(divider()).children(tracks)),
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

/// Where the song plays, as m:ss.d, read every [`CLOCK_TICK`] on the backend (the player may
/// be behind a JNI call) and shown as it comes.
#[derive(PartialEq)]
struct PlayClock;

impl Component for PlayClock {
    fn render(&self) -> impl IntoElement {
        let (c, d) = look();
        let nav = use_consume::<Nav>();
        let mut at = use_state(|| None::<u64>);
        use_hook(move || {
            let backend = nav.backend();
            let (tx, mut rx) = tokio::sync::mpsc::channel(1);
            backend.clone().spawn_task(async move {
                loop {
                    tokio::time::sleep(CLOCK_TICK).await;
                    // Gone with the clock: nobody receives.
                    if tx.send(backend.music().position()).await.is_err() {
                        return;
                    }
                }
            });
            spawn(async move {
                while let Some(ms) = rx.recv().await {
                    if *at.peek() != ms {
                        at.set(ms);
                    }
                }
            });
        });
        let text = at.read().map_or_else(String::new, |ms| {
            format!("{}:{:02}.{}", ms / 60_000, ms / 1000 % 60, ms / 100 % 10)
        });
        txt(text, d.font_sm(), c.text_muted, 500).max_lines(1)
    }
}

impl Component for QueuePanel {
    fn render(&self) -> impl IntoElement {
        let (c, d) = look();
        let nav = use_consume::<Nav>();
        let state = use_consume::<State<MusicState>>();
        let mut over = use_state(|| None::<Option<u64>>);
        let mut open = use_state(|| false);
        let unfolded = *open.read();
        let MusicState {
            queue,
            current,
            paused,
            loading,
            sharing,
            listeners,
            following,
            ..
        } = state.read().clone();
        let row_height = d.control_height() + 8.;

        let (title, mut detail) = match &current {
            Some(e) if loading => (e.name.clone(), format!("Starting… · {}", e.from_name)),
            Some(e) => (e.name.clone(), e.from_name.clone()),
            None => (
                "Nothing playing".to_owned(),
                "Press play to start the queue".to_owned(),
            ),
        };
        if let Some(f) = &following {
            detail = format!("Listening along with {} · {detail}", f.name);
        } else if sharing {
            detail = match listeners.as_slice() {
                [] => format!("Shared · {detail}"),
                [one] => format!("{one} listening along · {detail}"),
                many => format!("{} listening along · {detail}", and_list(many)),
            };
        }
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
                    .child(icon(
                        if unfolded {
                            Icon::ChevronDown
                        } else {
                            Icon::ChevronUp
                        },
                        18.,
                        c.text_faint,
                    ))
                    .child(
                        rect()
                            .width(Size::flex(1.))
                            .spacing(2.)
                            .child(txt(title, d.font_md(), c.text_strong, 650).elide())
                            .child(txt(detail, d.font_sm(), c.text_muted, 400).elide()),
                    ),
            )
            // Down to tenths: two devices listening along can be compared at a glance.
            .maybe(current.is_some() && !loading, |el| el.child(PlayClock))
            .child(
                IconButton::new(
                    if current.is_some() && !paused {
                        Icon::Pause
                    } else {
                        Icon::Play
                    },
                    move |_| with_music(nav, |m, b| m.toggle(b)),
                )
                .tint(c.accent_text),
            )
            .maybe(current.is_some(), |el| {
                el.child(IconButton::new(Icon::Skip, move |_| {
                    with_music(nav, |m, b| m.next(b))
                }))
            })
            // Share what plays, for the others to listen along; tinted while shared.
            .maybe(current.is_some() && following.is_none(), |el| {
                el.child(
                    IconButton::new(Icon::Broadcast, move |_| {
                        with_music(nav, |m, b| m.set_sharing(b, !sharing));
                        nav.toast(
                            if sharing {
                                "No longer shared"
                            } else {
                                "Shared with the group: they can listen along"
                            }
                            .to_owned(),
                        );
                    })
                    .tint(if sharing { c.accent_text } else { c.text_faint }),
                )
            })
            .maybe(following.is_some(), |el| {
                el.child(IconButton::new(Icon::Close, move |_| {
                    with_music(nav, |m, b| m.stop_following(b))
                }))
            });

        let rows =
            queue.iter().map(|e| {
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
                    .background(if highlighted {
                        c.chip_bg
                    } else {
                        Color::TRANSPARENT
                    })
                    .child(icon(Icon::Grip, 16., c.text_faint))
                    .child(
                        txt(e.name.clone(), d.font_md(), c.text_strong, 400)
                            .width(Size::flex(1.))
                            .elide(),
                    )
                    .child(txt(e.from_name.clone(), d.font_sm(), c.text_muted, 400).max_lines(1))
                    .child(
                        IconButton::new(Icon::Close, move |_| {
                            with_music(nav, |m, b| m.remove(b, key))
                        })
                        .size(row_height - 8.),
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
                        .drag_element(card().width(Size::px(260.)).child(
                            card_body(0.).padding((Dim::SPACE2, Dim::SPACE3)).child(
                                txt(e.name.clone(), d.font_md(), c.text_strong, 600).elide(),
                            ),
                        ))
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
                .background(if *over.read() == Some(None) {
                    c.chip_bg
                } else {
                    Color::TRANSPARENT
                }),
        );

        let shown = queue.len().min(QUEUE_ROWS) as f32;
        card().child(card_body(Dim::SPACE2).child(now).maybe(unfolded, |el| {
            el.child(divider())
                .child(
                    rect()
                        .horizontal()
                        .width(Size::fill())
                        .content(Content::flex())
                        .cross_align(Alignment::Center)
                        .child(
                            txt(
                                format!("Up next · {}", queue.len()),
                                d.font_xs(),
                                c.text_faint,
                                600,
                            )
                            .width(Size::flex(1.)),
                        )
                        .child(
                            Btn::ghost("Clear")
                                .icon(Icon::Trash)
                                .on_press(move |_| with_music(nav, |m, b| m.clear(b))),
                        ),
                )
                .child(
                    ScrollView::new()
                        .width(Size::fill())
                        .height(Size::px(shown * row_height + Dim::SPACE3))
                        .children(rows)
                        .child(end),
                )
        }))
    }
}
