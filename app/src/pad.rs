//! The scratchpad editor: a big plain-text field whose text the core replaces when someone else edits.
//! Freya's `Input` resets the cursor on such a change; this one keeps it where it belongs.

use std::time::Duration;

use flipflop::groupdoc::map_cursor;
use freya::{prelude::*, text_edit::*};
#[cfg(target_os = "android")]
use freya::radio::use_radio;

#[cfg(target_os = "android")]
use crate::model::{Channel, Ime, Model};
use crate::{
    icons::Icon,
    nav::Nav,
    pages::copy,
    theme::{Dim, look},
    widgets::{Btn, actions},
};

/// How far a finger may wander and still count as a tap (or a long press).
const TOUCH_SLOP: f64 = 8.;
/// How long a finger must stay put for a long press, which picks the word under it.
const LONG_PRESS: Duration = Duration::from_millis(450);

/// The touch in progress: where it started, and whether it became a drag.
#[derive(Clone, Copy, Default, PartialEq)]
struct Touch {
    from: Option<CursorPoint>,
    down: bool,
    /// Past [`TOUCH_SLOP`]: a drag, not a tap.
    moved: bool,
    /// Names the gesture, so a long-press timer from an earlier one does nothing.
    serial: u64,
}

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

/// Put `text` where the cursor is, over any selection, and leave the cursor after it. The document's text.
fn insert_at_cursor(editable: &mut UseEditable, text: &str) -> String {
    let mut editor = editable.editor_mut().write();
    editor.clear_preedit();
    let mut at = editor.cursor_pos();
    if let Some((from, to)) = editor.get_selection_range() {
        editor.remove(from..to);
        // `move_cursor_to` only moves the end of a selection: make it a cursor first, or the
        // pasted text stays selected.
        editor.clear_selection();
        editor.move_cursor_to(from);
        at = from;
    }
    editor.insert(text, at);
    editor.move_cursor_to(at + text.encode_utf16().count());
    editor.committed_text()
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
        let focused = focus() != Focus::Not;
        let mut dragging = use_state(|| false);
        let mut touch = use_state(Touch::default);
        let mut area = use_state(Area::default);
        let (mut movement, cursor_color) = use_cursor_blink(focused, c.accent);

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

        // What the Android IME committed since the last frame (typed, corrected or pasted) goes in
        // at the cursor. On every other platform this field gets ordinary key events instead.
        #[cfg(target_os = "android")]
        {
            let mut ime = use_radio::<Model, Channel>(Channel::Ime);
            let pending = if ime.read().ime.is_empty() {
                Vec::new()
            } else if focused {
                std::mem::take(&mut ime.write().ime)
            } else {
                // Nothing typed in the pad while it is not being edited belongs in it.
                ime.write().ime.clear();
                Vec::new()
            };
            for event in pending {
                match event {
                    Ime::Composing(text) => {
                        let mut editor = editable.editor_mut().write();
                        if text.is_empty() {
                            editor.clear_preedit();
                        } else {
                            editor.set_preedit(&text);
                        }
                    }
                    Ime::Text(text) => {
                        let after = insert_at_cursor(&mut editable, &text);
                        publish(after);
                    }
                    Ime::SelectAll => {
                        let mut editor = editable.editor_mut().write();
                        let end = editor.len_utf16_cu();
                        editor.set_selection((0, end));
                    }
                    Ime::Copy => {
                        if let Some(text) = editable.editor().peek().get_selected_text()
                            && !text.is_empty()
                        {
                            flipflop::platform::copy_text(&text);
                        }
                    }
                    Ime::Cut => {
                        let after = {
                            let mut editor = editable.editor_mut().write();
                            let Some((from, to)) = editor.get_selection_range() else {
                                continue;
                            };
                            if let Some(text) = editor.get_selected_text()
                                && !text.is_empty()
                            {
                                flipflop::platform::copy_text(&text);
                            }
                            editor.remove(from..to);
                            editor.clear_selection();
                            editor.move_cursor_to(from);
                            editor.committed_text()
                        };
                        publish(after);
                    }
                    key => {
                        let key = match key {
                            Ime::Backspace => Key::Named(NamedKey::Backspace),
                            Ime::Enter => Key::Named(NamedKey::Enter),
                            Ime::Left => Key::Named(NamedKey::ArrowLeft),
                            Ime::Right => Key::Named(NamedKey::ArrowRight),
                            Ime::Up => Key::Named(NamedKey::ArrowUp),
                            Ime::Down => Key::Named(NamedKey::ArrowDown),
                            Ime::Text(_) | Ime::Composing(_) | Ime::SelectAll | Ime::Copy | Ime::Cut => continue,
                        };
                        let before = editable.editor().peek().committed_text();
                        editable.process_event(EditableEvent::KeyDown {
                            key: &key,
                            modifiers: Modifiers::empty(),
                            editor_line: Some(EditorLine::SingleParagraph),
                            holder: Some(&holder.read()),
                        });
                        let after = editable.editor().peek().committed_text();
                        if after != before {
                            publish(after);
                        }
                    }
                }
            }
        }

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
        // On Android `on_focus_press` fires when the finger goes up. A field being edited places the
        // caret when the finger goes down (below), so only a tap on an unfocused pad gets here.
        let on_focus_press = move |e: Event<FocusPressEventData>| {
            e.stop_propagation();
            if cfg!(target_os = "android") {
                let wandered = touch.peek().moved;
                touch.write().moved = false;
                // A drag was a scroll, not a tap: leave the pad alone.
                if wandered || a11y_id.is_focused() {
                    return;
                }
            } else {
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
        // A finger going down on the pad being edited places the caret and starts selecting, the way
        // a text field does; on an unfocused pad it is left to the scroll view, and the tap focuses.
        let on_pointer_down = move |e: Event<PointerEventData>| {
            // Only Android fires `on_focus_press` on release; elsewhere it already handles the press
            // (and desktop touch screens keep that behavior).
            if !cfg!(target_os = "android") || !matches!(e.data(), PointerEventData::Touch(_)) {
                return;
            }
            let serial = touch.peek().serial + 1;
            touch.set(Touch {
                from: Some(e.global_location()),
                down: true,
                moved: false,
                serial,
            });
            if !a11y_id.is_focused() {
                return;
            }
            e.stop_propagation();
            dragging.set_if_modified(true);
            movement.reset();
            if editable.editor().peek().rope().len_chars() > 0 {
                editable.process_event(EditableEvent::Down {
                    location: e.element_location(),
                    editor_line: EditorLine::SingleParagraph,
                    holder: &holder.read(),
                });
            }
            // Held still, it picks the word under the finger, like the platform does.
            let mut held = editable;
            spawn(async move {
                tokio::time::sleep(LONG_PRESS).await;
                let now = *touch.peek();
                if now.serial != serial || !now.down || now.moved || !a11y_id.is_focused() {
                    return;
                }
                let mut editor = held.editor_mut().write();
                let at = editor.cursor_pos();
                let (mut from, mut to) = editor.find_word_boundaries(at);
                // At the very end there is no word after the cursor: pick the one before it.
                if from == to && at > 0 && at >= editor.len_utf16_cu() {
                    (from, to) = editor.find_word_boundaries(at - 1);
                }
                if from != to {
                    editor.set_selection((from, to));
                }
            });
        };
        let on_global_pointer_move = move |e: Event<PointerEventData>| {
            // A finger that has wandered is dragging (and no longer a long press); a tap does not move.
            let now = *touch.peek();
            if now.down
                && !now.moved
                && let Some(from) = now.from
            {
                let moved = (e.global_location() - from).abs();
                if moved.x > TOUCH_SLOP || moved.y > TOUCH_SLOP {
                    touch.write().moved = true;
                }
            }
            let dragging = *dragging.peek();
            // Touch selection starts after the slop, so a tap does not select a letter by accident.
            if a11y_id.is_focused()
                && dragging
                && (touch.peek().moved || !cfg!(target_os = "android"))
                && editable.editor().peek().rope().len_chars() > 0
            {
                let location = (e.global_location() - area.read().min().to_f64()).to_point();
                editable.process_event(EditableEvent::Move {
                    location,
                    editor_line: EditorLine::SingleParagraph,
                    holder: &holder.read(),
                });
            }
        };
        let on_global_pointer_up = move |_: Event<PointerEventData>| {
            {
                let mut now = touch.write();
                now.down = false;
                now.from = None;
                // A long press that has not picked its word yet is over.
                now.serial += 1;
            }
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
            let text = insert_at_cursor(&mut editable, &pasted);
            publish_paste(text);
        };

        // The Android IME edits a hidden native field (see `startImeBridge`) while the pad has the
        // focus: that is what lets it paste, autocorrect and compose.
        #[cfg(target_os = "android")]
        {
            use_side_effect_with_deps(&focused, |focused| {
                if *focused {
                    flipflop::platform::start_ime_bridge();
                } else {
                    flipflop::platform::stop_ime_bridge();
                }
            });
            use_drop(flipflop::platform::stop_ime_bridge);
        }
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

        // Copy what is on screen, which can be a keystroke ahead of the document's copy.
        let copy_all = {
            let editable = editable;
            move |_| copy(nav, &editable.editor().peek().committed_text())
        };
        // Paste into the editor (at the cursor, over a selection) and publish that: the field shows it
        // at once, instead of waiting for the document to come back with the text appended.
        let paste = {
            let mut editable = editable;
            let publish = publish.clone();
            move |_| {
                let pasted = flipflop::platform::paste_text();
                if pasted.is_empty() {
                    return nav.toast("Nothing to paste".into());
                }
                a11y_id.request_focus();
                let text = insert_at_cursor(&mut editable, &pasted);
                publish(text);
                nav.toast("Pasted".into());
            }
        };

        let editor = rect()
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
                // While the pad is being edited a finger selects (see `on_pointer_down`), so the
                // scroll view must not take the focus and the keyboard away when it is dragged.
                ScrollView::new().expanded().drag_scrolling(!focused).child(
                    paragraph()
                        .holder(holder.read().clone())
                        .on_sized(move |e: Event<SizedEventData>| area.set_if_modified(e.area))
                        .on_pointer_down(on_pointer_down)
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
            );

        rect()
            .expanded()
            .spacing(Dim::SPACE2)
            .content(Content::flex())
            .child(editor)
            .child(
                actions()
                    .child(Btn::secondary("Copy all").icon(Icon::Copy).on_press(copy_all))
                    .child(Btn::secondary("Paste").icon(Icon::Clipboard).on_press(paste)),
            )
    }

    fn render_key(&self) -> DiffKey {
        DiffKey::from(&self.gid)
    }
}
