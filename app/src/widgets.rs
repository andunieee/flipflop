//! The app's building blocks: text styles, cards, buttons, badges, fields.

use std::{cell::RefCell, collections::HashMap};

use freya::{elements::image::ImageHandle, engine::prelude::AlphaType, prelude::*};

use crate::{
    icons::Icon,
    model::Tone,
    theme::{Dim, look},
};

/// Text in the app's defaults: `size`, `color`, `weight`; wraps unless [`Elide::elide`]d.
pub fn txt(text: impl Into<String>, size: f32, color: Color, weight: i32) -> Label {
    label()
        .text(text.into())
        .font_size(size)
        .color(color)
        .font_weight(weight)
}

pub trait Elide {
    /// One line, cut with an ellipsis.
    fn elide(self) -> Self;
}

impl Elide for Label {
    fn elide(self) -> Self {
        self.max_lines(1).text_overflow(TextOverflow::Ellipsis)
    }
}

pub fn icon(icon: Icon, size: f32, color: Color) -> SvgViewer {
    SvgViewer::new((icon, icon.svg()))
        .stroke(color)
        .width(Size::px(size))
        .height(Size::px(size))
}

pub fn page_title(title: impl Into<String>, subtitle: impl Into<String>) -> Rect {
    let (c, d) = look();
    let subtitle = subtitle.into();
    rect()
        .width(Size::flex(1.))
        .spacing(Dim::SPACE1)
        .child(txt(title, d.font_xl(), c.text_strong, 700).elide())
        .maybe(!subtitle.is_empty(), |el| {
            el.child(txt(subtitle, d.font_sm(), c.text_muted, 400))
        })
}

/// Small all-caps label above a list.
pub fn overline(text: &str) -> Label {
    let (c, d) = look();
    txt(text, d.font_xs(), c.text_faint, 650).letter_spacing(0.6)
}

pub fn hint(text: impl Into<String>) -> Label {
    let (c, d) = look();
    txt(text, d.font_sm(), c.text_muted, 400).width(Size::fill())
}

pub fn hint_center(text: impl Into<String>) -> Label {
    hint(text).text_align(TextAlign::Center)
}

pub fn divider() -> Rect {
    let (c, _) = look();
    rect()
        .width(Size::fill())
        .height(Size::px(1.))
        .background(c.border)
}

pub fn dot(online: bool, size: f32) -> Rect {
    let (c, _) = look();
    dot_colored(if online { c.online } else { c.offline }, size)
}

pub fn dot_colored(color: Color, size: f32) -> Rect {
    rect()
        .width(Size::px(size))
        .height(Size::px(size))
        .corner_radius(size / 2.)
        .background(color)
}

/// A card: white (or dark) with a border.
pub fn card() -> Rect {
    let (c, _) = look();
    card_with(c.card_bg, c.border, 1.)
}

pub fn card_with(background: Color, border: Color, width: f32) -> Rect {
    rect()
        .width(Size::fill())
        .corner_radius(Dim::RADIUS_LG)
        .background(background)
        .border(
            Border::new()
                .fill(border)
                .width(width)
                .alignment(BorderAlignment::Inner),
        )
}

/// A card's padded column.
pub fn card_body(spacing: f32) -> Rect {
    let (_, d) = look();
    rect()
        .width(Size::fill())
        .padding(d.card_padding())
        .spacing(spacing)
}

/// Pill with a tinted background.
pub fn badge(text: impl Into<String>, tone: Tone) -> Rect {
    let (c, d) = look();
    let (bg, fg) = match tone {
        Tone::Ok => (c.success_soft, c.success),
        Tone::Error => (c.error_soft, c.error),
        Tone::Warn => (c.warn_soft, c.warn),
        Tone::Active => (c.info_soft, c.info),
        Tone::Muted => (c.chip_bg, c.text_muted),
    };
    rect()
        .corner_radius(99.)
        .background(bg)
        .padding((2., 8.))
        .child(txt(text, d.font_xs(), fg, 600).max_lines(1))
}

/// Round tile with an icon (file kinds, section headers).
pub fn icon_tile(i: Icon, size: f32, tint: Color, ink: Color) -> Rect {
    rect()
        .width(Size::px(size))
        .height(Size::px(size))
        .corner_radius(size * 0.3)
        .background(tint)
        .center()
        .child(icon(i, size * 0.5, ink))
}

pub fn progress_bar(value: f32) -> Rect {
    let (c, _) = look();
    rect()
        .width(Size::fill())
        .height(Size::px(6.))
        .corner_radius(3.)
        .background(c.inset_bg)
        .child(
            rect()
                .width(Size::percent(value.clamp(0., 1.) * 100.))
                .height(Size::fill())
                .corner_radius(3.)
                .background(c.accent),
        )
}

/// A row of buttons, pushed to the end.
pub fn actions() -> Rect {
    rect()
        .horizontal()
        .width(Size::fill())
        .main_align(Alignment::End)
        .cross_align(Alignment::Center)
        .spacing(Dim::SPACE2)
}

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

#[derive(PartialEq)]
pub struct Btn {
    label: String,
    kind: Kind,
    icon: Option<Icon>,
    enabled: bool,
    expanded: bool,
    on_press: Option<EventHandler<()>>,
}

impl Btn {
    pub fn new(kind: Kind, label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            kind,
            icon: None,
            enabled: true,
            expanded: false,
            on_press: None,
        }
    }
    pub fn primary(label: impl Into<String>) -> Self {
        Self::new(Kind::Primary, label)
    }
    pub fn secondary(label: impl Into<String>) -> Self {
        Self::new(Kind::Secondary, label)
    }
    pub fn ghost(label: impl Into<String>) -> Self {
        Self::new(Kind::Ghost, label)
    }
    pub fn danger(label: impl Into<String>) -> Self {
        Self::new(Kind::Danger, label)
    }
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
    /// Share the row's width with the other expanded buttons.
    pub fn expanded(mut self) -> Self {
        self.expanded = true;
        self
    }
    pub fn on_press(mut self, f: impl FnMut(()) + 'static) -> Self {
        self.on_press = Some(EventHandler::new(f));
        self
    }
}

impl Component for Btn {
    fn render(&self) -> impl IntoElement {
        let (c, d) = look();
        let mut hovered = use_state(|| false);
        let (fill, hover, ink, border) = match self.kind {
            Kind::Primary => (c.accent, c.accent_hover, Color::WHITE, None),
            Kind::Secondary => (c.chip_bg, c.chip_bg_hover, c.text_strong, Some(c.border_strong)),
            Kind::Ghost => (Color::TRANSPARENT, c.chip_bg, c.text, None),
            Kind::Danger => (Color::TRANSPARENT, c.error_soft, c.error, None),
        };
        let enabled = self.enabled;
        let on_press = self.on_press.clone();
        rect()
            .horizontal()
            .center()
            .min_height(Size::px(d.control_height()))
            .min_width(Size::px(d.control_height()))
            .maybe(self.expanded, |el| el.width(Size::flex(1.)))
            .padding((0., if self.label.is_empty() { 0. } else { d.button_padding() }))
            .spacing(if d.narrow { 6. } else { Dim::SPACE2 })
            .corner_radius(Dim::RADIUS_SM + 2.)
            .background(if enabled && *hovered.read() { hover } else { fill })
            .map(border, |el, border| {
                el.border(
                    Border::new()
                        .fill(border)
                        .width(1.)
                        .alignment(BorderAlignment::Inner),
                )
            })
            .maybe(!enabled, |el| el.opacity(0.45))
            .cursor(if enabled { CursorIcon::Pointer } else { CursorIcon::Default })
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set(false))
            .on_press(move |e: Event<PressEventData>| {
                e.stop_propagation();
                if enabled && let Some(f) = &on_press {
                    f.call(());
                }
            })
            .map(self.icon, |el, i| el.child(icon(i, 18., ink)))
            .maybe(!self.label.is_empty(), |el| {
                el.child(txt(self.label.clone(), d.font_md(), ink, 600).max_lines(1))
            })
    }
}

/// A round button with just an icon.
#[derive(PartialEq)]
pub struct IconButton {
    icon: Icon,
    tint: Option<Color>,
    danger: bool,
    size: Option<f32>,
    on_press: EventHandler<()>,
}

impl IconButton {
    pub fn new(icon: Icon, on_press: impl FnMut(()) + 'static) -> Self {
        Self {
            icon,
            tint: None,
            danger: false,
            size: None,
            on_press: EventHandler::new(on_press),
        }
    }
    pub fn tint(mut self, tint: Color) -> Self {
        self.tint = Some(tint);
        self
    }
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }
    pub fn size(mut self, size: f32) -> Self {
        self.size = Some(size);
        self
    }
}

impl Component for IconButton {
    fn render(&self) -> impl IntoElement {
        let (c, d) = look();
        let mut hovered = use_state(|| false);
        let size = self.size.unwrap_or(d.control_height());
        let bg = match (*hovered.read(), self.danger) {
            (false, _) => Color::TRANSPARENT,
            (true, true) => c.error_soft,
            (true, false) => c.chip_bg,
        };
        let ink = if self.danger {
            c.error
        } else {
            self.tint.unwrap_or(c.text_muted)
        };
        let on_press = self.on_press.clone();
        rect()
            .width(Size::px(size))
            .height(Size::px(size))
            .corner_radius(size / 2.)
            .background(bg)
            .center()
            .cursor(CursorIcon::Pointer)
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set(false))
            .on_press(move |e: Event<PressEventData>| {
                e.stop_propagation();
                on_press.call(());
            })
            .child(icon(self.icon, if d.touch { 20. } else { 17. }, ink))
    }
}

/// A card that darkens a little under the pointer.
#[derive(PartialEq)]
pub struct ClickCard {
    border: Option<(Color, f32)>,
    children: Vec<Element>,
    on_press: EventHandler<()>,
    key: DiffKey,
}

impl ClickCard {
    pub fn new(on_press: impl FnMut(()) + 'static) -> Self {
        Self {
            border: None,
            children: vec![],
            on_press: EventHandler::new(on_press),
            key: DiffKey::None,
        }
    }
    pub fn border(mut self, color: Color, width: f32) -> Self {
        self.border = Some((color, width));
        self
    }
}

impl ChildrenExt for ClickCard {
    fn children_mut(&mut self) -> &mut Vec<Element> {
        &mut self.children
    }
}

impl KeyExt for ClickCard {
    fn key_mut(&mut self) -> &mut DiffKey {
        &mut self.key
    }
}

impl Component for ClickCard {
    fn render(&self) -> impl IntoElement {
        let (c, _) = look();
        let mut hovered = use_state(|| false);
        let (border, width) = self.border.unwrap_or((c.border, 1.));
        let background = if *hovered.read() {
            c.inset_bg
        } else {
            c.card_bg
        };
        let on_press = self.on_press.clone();
        card_with(background, border, width)
            .cursor(CursorIcon::Pointer)
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set(false))
            .on_press(move |_| on_press.call(()))
            .children(self.children.clone())
    }

    fn render_key(&self) -> DiffKey {
        self.key.clone().or(self.default_key())
    }
}

/// On/off switch with a label and an explanation under it.
#[derive(PartialEq)]
pub struct ToggleRow {
    label: String,
    hint: String,
    on: bool,
    on_toggle: EventHandler<bool>,
}

impl ToggleRow {
    pub fn new(
        label: impl Into<String>,
        hint: impl Into<String>,
        on: bool,
        on_toggle: impl FnMut(bool) + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            hint: hint.into(),
            on,
            on_toggle: EventHandler::new(on_toggle),
        }
    }
}

impl Component for ToggleRow {
    fn render(&self) -> impl IntoElement {
        let (c, d) = look();
        let (on, on_toggle) = (self.on, self.on_toggle.clone());
        let toggle = on_toggle.clone();
        rect()
            .horizontal()
            .width(Size::fill())
            .content(Content::flex())
            .min_height(Size::px(d.control_height()))
            .padding((4., 0.))
            .spacing(Dim::SPACE3)
            .cross_align(Alignment::Center)
            .cursor(CursorIcon::Pointer)
            .on_press(move |_| on_toggle.call(!on))
            .child(
                rect()
                    .width(Size::flex(1.))
                    .spacing(2.)
                    .child(txt(self.label.clone(), d.font_md(), c.text_strong, 600))
                    .child(txt(self.hint.clone(), d.font_sm(), c.text_muted, 400)),
            )
            .child(Switch::new(WritableSetter::new(on, move |v| toggle.call(v))))
    }
}

/// Segmented tab strip.
#[derive(PartialEq)]
pub struct TabStrip {
    tabs: Vec<(String, String)>,
    current: String,
    on_pick: EventHandler<String>,
}

impl TabStrip {
    /// `tabs`: (key, label).
    pub fn new(
        tabs: Vec<(String, String)>,
        current: impl Into<String>,
        on_pick: impl FnMut(String) + 'static,
    ) -> Self {
        Self {
            tabs,
            current: current.into(),
            on_pick: EventHandler::new(on_pick),
        }
    }
}

impl Component for TabStrip {
    fn render(&self) -> impl IntoElement {
        let (c, d) = look();
        rect()
            .horizontal()
            .width(Size::fill())
            .height(Size::px(d.control_height() + 8.))
            .content(Content::flex())
            .corner_radius(Dim::RADIUS)
            .background(c.chip_bg)
            .padding(4.)
            .spacing(4.)
            .children(self.tabs.iter().map(|(key, title)| {
                let on = *key == self.current;
                let (on_pick, key) = (self.on_pick.clone(), key.clone());
                rect()
                    .width(Size::flex(1.))
                    .height(Size::fill())
                    .center()
                    .corner_radius(Dim::RADIUS_SM + 2.)
                    .maybe(on, |el| {
                        el.background(c.card_bg).border(
                            Border::new()
                                .fill(c.border)
                                .width(1.)
                                .alignment(BorderAlignment::Inner),
                        )
                    })
                    .cursor(CursorIcon::Pointer)
                    .on_press(move |_| on_pick.call(key.clone()))
                    .child(
                        txt(
                            title.clone(),
                            d.font_md(),
                            if on { c.accent_text } else { c.text_muted },
                            if on { 700 } else { 500 },
                        )
                        .elide(),
                    )
                    .into_element()
            }))
    }
}

/// A single-line text field. `value` is the text shown; edits go to `on_change`.
pub fn field(
    value: impl IntoReadable<String>,
    placeholder: &'static str,
    on_change: impl FnMut(String) + 'static,
) -> Input {
    let (_, d) = look();
    Input::new(WritableSetter::new(value, EventHandler::new(on_change)))
        .placeholder(placeholder)
        .width(Size::fill())
        .height(Size::px(d.control_height()))
}

/// A multi-line text field (snippets).
pub fn area(
    value: impl IntoReadable<String>,
    placeholder: &'static str,
    on_change: impl FnMut(String) + 'static,
) -> Input {
    let (_, d) = look();
    Input::new(WritableSetter::new(value, EventHandler::new(on_change)))
        .placeholder(placeholder)
        .multiline(true)
        .width(Size::fill())
        .height(Size::px(if d.touch { 96. } else { 72. }))
}

/// Text to select and copy (tickets, links), in a field-like box.
pub fn copyable(text: impl Into<String>) -> Rect {
    let (c, d) = look();
    rect()
        .width(Size::fill())
        .min_height(Size::px(d.control_height()))
        .main_align(Alignment::Center)
        .padding((6., Dim::SPACE3))
        .corner_radius(Dim::RADIUS_SM + 2.)
        .background(c.inset_bg)
        .border(
            Border::new()
                .fill(c.border_strong)
                .width(1.)
                .alignment(BorderAlignment::Inner),
        )
        .child(
            SelectableText::new()
                .span(Span::new(text.into()))
                .font_size(d.font_md())
                .color(c.text_strong)
                .width(Size::fill()),
        )
}

thread_local! {
    /// QR codes already drawn, by text.
    static QR_CACHE: RefCell<HashMap<String, ImageHandle>> = RefCell::default();
}

/// `text` as a QR code, black on white, `size` pixels square.
pub fn qr_code(text: &str, size: f32) -> Rect {
    let handle = QR_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(handle) = cache.get(text) {
            return Some(handle.clone());
        }
        let (w, h, rgba) = flipflop::qr::render(text, 8)?;
        let handle = ImageHandle::from_rgba(w, h, rgba.into(), AlphaType::Opaque)?;
        if cache.len() > 8 {
            cache.clear();
        }
        cache.insert(text.to_owned(), handle.clone());
        Some(handle)
    });
    rect()
        .width(Size::px(size))
        .height(Size::px(size))
        .corner_radius(Dim::RADIUS)
        .background(Color::WHITE)
        .padding(4.)
        .maybe_child(
            handle.map(|h| image(h).width(Size::fill()).height(Size::fill())),
        )
}

/// A dimmed backdrop over the whole window; pressing it calls `on_dismiss`. `content` must stop
/// presses on itself from reaching the backdrop (see [`sheet`]).
pub fn scrim(on_dismiss: impl FnMut(Event<PressEventData>) + 'static, content: impl IntoElement) -> Rect {
    rect()
        .layer(Layer::Overlay)
        .position(Position::new_global())
        .width(Size::window_percent(100.))
        .height(Size::window_percent(100.))
        .background(Color::BLACK.with_a(140))
        .cross_align(Alignment::Center)
        .on_press(on_dismiss)
        .child(content)
}

/// A panel on a [`scrim`] that keeps presses to itself.
pub fn sheet() -> Rect {
    rect().on_press(|e: Event<PressEventData>| e.stop_propagation())
}
