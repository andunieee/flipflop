//! The scratchpad editor: a big plain-text field whose text the core replaces when someone else edits.
//! Freya's `Input` resets the cursor on such a change; this one keeps it where it belongs.

use flipflop::groupdoc::map_cursor;
use freya::{prelude::*, text_edit::*};

use crate::{
    nav::Nav,
    theme::{Dim, look},
};

/// The editor counts in UTF-16 code units, the document in characters.
fn utf16_to_char(text: &str, pos: usize) -> usize {
    let mut units = 0;
    for (i, c) in text.chars().enumerate() {
        if units >= pos {
            return i;
        }
        units += c.len_utf16();
    }
    text.chars().count()
}

fn char_to_utf16(text: &str, idx: usize) -> usize {
    text.chars().take(idx).map(char::len_utf16).sum()
}

/// Where a position (UTF-16) in `old` ends up in `new`.
fn remap(old: &str, new: &str, pos: usize) -> usize {
    char_to_utf16(new, map_cursor(old, new, utf16_to_char(old, pos)))
}

#[derive(PartialEq)]
pub struct PadEditor {
    pub gid: String,
    /// The scratchpad's text in the document.
    pub text: String,
    /// `text` is our own last edit, already on screen.
    pub local: bool,
}

impl Component for PadEditor {
    fn render(&self) -> impl IntoElement {
        let (c, d) = look();
        let nav = use_consume::<Nav>();
        let holder = use_state(ParagraphHolder::default);
        let mut editable = use_editable(|| self.text.clone(), EditableConfig::new);
        let a11y_id = use_a11y();
        let focus = use_focus(a11y_id);
        let mut dragging = use_state(|| false);
        let mut area = use_state(Area::default);
        let (mut movement, cursor_color) = use_cursor_blink(focus() != Focus::Not, c.accent);

        // Someone else edited: take their text, and keep the cursor (and selection) on the same characters.
        let shown = editable.editor().peek().committed_text();
        if !self.local && self.text != shown {
            let mut editor = editable.editor_mut().write();
            let selection = editor.get_selection_range();
            let cursor = editor.cursor_pos();
            editor.clear_preedit();
            editor.set(&self.text);
            editor.editor_history_mut().clear();
            match selection {
                Some((from, to)) => editor.set_selection((
                    remap(&shown, &self.text, from),
                    remap(&shown, &self.text, to),
                )),
                None => editor.move_cursor_to(remap(&shown, &self.text, cursor)),
            }
        }

        let gid = self.gid.clone();
        let publish = move |text: String| {
            let gid = gid.clone();
            nav.backend().run(move |core| {
                core.set_scratch(&gid, &text);
                Ok(())
            });
        };
        let publish_key = publish.clone();
        let publish_paste = publish.clone();

        let on_key_down = move |e: Event<KeyboardEventData>| {
            if e.key == Key::Named(NamedKey::Escape) {
                a11y_id.request_unfocus();
                return;
            }
            e.stop_propagation();
            movement.reset();
            let before = editable.editor().peek().committed_text();
            editable.process_event(EditableEvent::KeyDown {
                key: &e.key,
                modifiers: e.modifiers,
                editor_line: Some(EditorLine::SingleParagraph),
                holder: Some(&holder.read()),
            });
            let after = editable.editor().peek().committed_text();
            if after != before {
                publish_key(after);
            }
        };
        let on_key_up = move |e: Event<KeyboardEventData>| {
            e.stop_propagation();
            editable.process_event(EditableEvent::KeyUp { key: &e.key });
        };
        let on_ime_preedit = move |e: Event<ImePreeditEventData>| {
            let mut editor = editable.editor_mut().write();
            if e.data().text.is_empty() {
                editor.clear_preedit();
            } else {
                editor.set_preedit(&e.data().text);
            }
        };
        let on_focus_press = move |e: Event<FocusPressEventData>| {
            e.stop_propagation();
            // On a touch screen the first tap focuses, the next ones place the cursor and select.
            if !cfg!(target_os = "android") || a11y_id.is_focused() {
                dragging.set_if_modified(true);
            }
            movement.reset();
            // An empty pad shows its placeholder in the paragraph: no position in it is one in the text.
            if editable.editor().peek().rope().len_chars() > 0 {
                editable.process_event(EditableEvent::Down {
                    location: e.element_location(),
                    editor_line: EditorLine::SingleParagraph,
                    holder: &holder.read(),
                });
            }
            a11y_id.request_focus();
        };
        let on_global_pointer_move = move |e: Event<PointerEventData>| {
            if a11y_id.is_focused() && *dragging.read() && editable.editor().peek().rope().len_chars() > 0 {
                let location = (e.global_location() - area.read().min().to_f64()).to_point();
                editable.process_event(EditableEvent::Move {
                    location,
                    editor_line: EditorLine::SingleParagraph,
                    holder: &holder.read(),
                });
            }
        };
        let on_global_pointer_up = move |_: Event<PointerEventData>| {
            if *dragging.peek() {
                editable.process_event(EditableEvent::Release);
                dragging.set(false);
                // Selecting text copies it to the primary selection on Linux.
                #[cfg(not(target_os = "android"))]
                if let Some(text) = editable.editor().peek().get_selected_text()
                    && !text.is_empty()
                {
                    flipflop::platform::copy_primary(&text);
                }
            }
        };
        // Middle-click pastes the primary selection on Linux, where the pointer is.
        let on_pointer_press = move |e: Event<PointerEventData>| {
            if e.button() != Some(MouseButton::Middle) {
                return;
            }
            e.stop_propagation();
            #[cfg(not(target_os = "android"))]
            let pasted = flipflop::platform::paste_primary();
            #[cfg(target_os = "android")]
            let pasted = String::new();
            if pasted.is_empty() {
                return;
            }
            a11y_id.request_focus();
            let text = {
                let mut editor = editable.editor_mut().write();
                let at = editor.cursor_pos();
                editor.insert(&pasted, at);
                editor.move_cursor_to(at + pasted.encode_utf16().count());
                editor.committed_text()
            };
            publish_paste(text);
        };

        let focused = focus() != Focus::Not;
        let (text, cursor, selection, preedit) = {
            let editor = editable.editor().read();
            (
                editor.committed_text(),
                focused.then(|| editor.cursor_pos()),
                editor
                    .get_visible_selection(EditorLine::SingleParagraph)
                    .filter(|_| focused),
                editor.has_preedit().then(|| editor.preedit_text_segments()),
            )
        };
        let empty = text.is_empty() && preedit.is_none();

        rect()
            .width(Size::fill())
            .height(Size::flex(1.))
            .corner_radius(Dim::RADIUS_LG)
            .background(c.card_bg)
            .border(
                Border::new()
                    .fill(if focused { c.accent } else { c.border })
                    .width(1.)
                    .alignment(BorderAlignment::Inner),
            )
            .a11y_id(a11y_id)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::MultilineTextInput)
            .a11y_alt(text.clone())
            .on_key_down(on_key_down)
            .on_key_up(on_key_up)
            .on_ime_preedit(on_ime_preedit)
            .on_pointer_press(on_pointer_press)
            .on_global_pointer_move(on_global_pointer_move)
            .on_global_pointer_up(on_global_pointer_up)
            .cursor(CursorIcon::Text)
            .child(
                ScrollView::new().expanded().child(
                    paragraph()
                        .holder(holder.read().clone())
                        .on_sized(move |e: Event<SizedEventData>| area.set_if_modified(e.area))
                        .on_focus_press(on_focus_press)
                        .width(Size::fill())
                        .min_height(Size::px(200.))
                        .padding(d.card_padding())
                        .font_size(d.font_md())
                        .color(if empty { c.text_faint } else { c.text_strong })
                        .cursor_index(cursor)
                        .cursor_color(cursor_color)
                        .highlights(selection.map(|s| vec![s]))
                        .map(preedit, |el, (before, preedit, after)| {
                            el.span(before)
                                .span(Span::new(preedit).text_decoration(TextDecoration::Underline))
                                .span(after)
                        })
                        .maybe(empty && !focused, |el| el.span("Everyone in the group can edit this field."))
                        .maybe(!empty && !editable.editor().read().has_preedit(), |el| el.span(text)),
                ),
            )
    }

    fn render_key(&self) -> DiffKey {
        DiffKey::from(&self.gid)
    }
}
