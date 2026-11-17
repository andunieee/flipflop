//! A Freya renderer for hypereffect, over `hypereffect::client::Client`: the client keeps the
//! element tree, applies signals, evaluates CEL and resolves every attribute; this module draws the
//! resolved tree with the app's own widgets and sends actions back to the wasm app.

use std::{cell::RefCell, rc::Rc, sync::Arc};

use base64::Engine;
use ciborium::Value as Cbor;
use flipflop::{
    apps::{App as WasmApp, DocHost, Runtime},
    core::Core,
    groupdoc::GroupDoc,
};
use freya::{prelude::*, radio::use_radio};
use hypereffect::{
    Component as HeComponent, Value as Json,
    client::{Client, ResolvedTree},
    json,
    message::RendererMessage,
};

use crate::{
    backend::Backend,
    model::{Channel, Model, Tone},
    theme::{Dim, look},
    widgets::*,
};

/// One app session: the wasm app, the renderer core, and the group document it writes to.
pub struct Session {
    backend: Option<Backend>,
    gid: String,
    app: WasmApp,
    client: Client,
    pub error: Option<String>,
}

struct CoreDoc {
    core: Core,
    gid: String,
}

impl DocHost for CoreDoc {
    fn with_doc(&self, f: &mut dyn FnMut(&mut GroupDoc)) {
        if self.core.with_group_doc(&self.gid, |d| f(d)).is_none() {
            // The group went away; let the app run on a throwaway document rather than panic.
            let mut doc = GroupDoc::new();
            f(&mut doc);
        }
    }
}

impl Session {
    pub fn open(
        backend: Backend,
        gid: String,
        app_id: String,
        hash: String,
    ) -> Result<Session, String> {
        let core = backend
            .core()
            .ok_or_else(|| "The node is still starting.".to_owned())?;
        let wasm = core
            .app_wasm(&hash)
            .ok_or_else(|| "The app's WebAssembly is not on this device yet.".to_owned())?;
        let host: Arc<dyn DocHost> = Arc::new(CoreDoc {
            core: core.clone(),
            gid: gid.clone(),
        });
        let mut session = Session::new(&wasm, &app_id, &core.me_id(), host, Some(backend), gid)?;
        if session.start()? {
            session.wrote();
        }
        Ok(session)
    }

    fn new(
        wasm: &[u8],
        app_id: &str,
        me: &str,
        host: Arc<dyn DocHost>,
        backend: Option<Backend>,
        gid: String,
    ) -> Result<Session, String> {
        let app = Runtime::default()
            .load(wasm, app_id, me, host)
            .map_err(|e| format!("The app did not start: {e:#}"))?;
        let mut client = Client::new();
        let _ = client.hello();
        Ok(Session {
            backend,
            gid,
            app,
            client,
            error: None,
        })
    }

    /// Send the app its start event and draw what it makes. Returns whether it changed the document.
    fn start(&mut self) -> Result<bool, String> {
        let made = self
            .app
            .start()
            .map_err(|e| format!("The app did not start: {e:#}"))?;
        self.frames();
        Ok(made)
    }

    /// A session with no node behind it, for tests: the app runs on the `host` it is given.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn standalone(
        app_id: &str,
        wasm: &[u8],
        me: &str,
        host: Arc<dyn DocHost>,
    ) -> Result<Session, String> {
        let mut session = Session::new(wasm, app_id, me, host, None, String::new())?;
        session.start()?;
        Ok(session)
    }

    fn wrote(&self) {
        // Publish and sync from the node's runtime, where `doc_changed` can spawn.
        let Some(backend) = &self.backend else {
            return;
        };
        let gid = self.gid.clone();
        backend.run(move |core| {
            core.app_wrote(&gid);
            Ok(())
        });
    }

    /// The document changed: let the app react, and feed whatever it draws to the renderer.
    pub fn pump(&mut self) {
        if self.app.is_stopped() {
            return;
        }
        for _ in 0..32 {
            match self.app.notify() {
                Ok(wrote) => {
                    self.frames();
                    if wrote {
                        self.wrote();
                    } else {
                        break;
                    }
                }
                Err(e) => {
                    self.fail(e);
                    break;
                }
            }
        }
    }

    /// Run a handler on element `id`, then let the app write the document and redraw.
    pub fn act(&mut self, id: &str, event: &str) {
        let Some(line) = self.client.trigger(id, event) else {
            self.report();
            return;
        };
        if self.app.is_stopped() {
            return;
        }
        let Ok(RendererMessage::Action(action)) = RendererMessage::parse(&line) else {
            return;
        };
        let payload = action.payload.map(json_to_cbor).unwrap_or(Cbor::Null);
        let signals = json_to_cbor(Json::Object(action.signals));
        match self.app.action(&action.name, payload, signals) {
            Ok(wrote) => {
                self.frames();
                if wrote {
                    self.wrote();
                }
                self.pump();
            }
            Err(e) => self.fail(e),
        }
        self.report();
    }

    pub fn write(&mut self, path: &str, value: Json) {
        self.client.write(path, value);
    }

    pub fn value(&self, path: &str) -> Option<&Json> {
        self.client.value(path)
    }

    pub fn tree(&self) -> Vec<ResolvedTree> {
        self.client.tree()
    }

    /// Record why the app failed. A stopped app keeps the first reason, not "the app was stopped".
    fn fail(&mut self, e: anyhow::Error) {
        if self.error.is_none() {
            self.error = Some(format!("{e:#}"));
        }
    }

    fn frames(&mut self) {
        let lines = self.app.take_ui();
        if !lines.is_empty() {
            self.client.receive(&lines);
        }
        self.report();
    }

    fn report(&mut self) {
        for diagnostic in self.client.take_diagnostics() {
            tracing::warn!("hypereffect: {diagnostic}");
        }
        for url in self.client.take_opens() {
            flipflop::platform::open_url(&url);
        }
    }
}

/// The app's interface. `rev` is bumped after local actions so Freya redraws; group changes arrive
/// through the model and are pumped here before drawing.
#[derive(Clone)]
pub struct HyperView {
    pub session: Rc<RefCell<Session>>,
    pub rev: State<u64>,
}

impl PartialEq for HyperView {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.session, &other.session) && self.rev == other.rev
    }
}

impl Component for HyperView {
    fn render(&self) -> impl IntoElement {
        let _ = self.rev.read();
        // Every group document change is published to the UI; subscribe so the app is pumped when a
        // peer's writes arrive, not only when the local user acts.
        let groups = use_radio::<Model, Channel>(Channel::Groups);
        let _groups = groups.read();

        if let Ok(mut session) = self.session.try_borrow_mut() {
            session.pump();
        }
        let (tree, error) = {
            let session = self.session.borrow();
            (session.tree(), session.error.clone())
        };
        let session = self.session.clone();
        let rev = self.rev;

        ScrollView::new().expanded().spacing(Dim::SPACE3).child(
            rect()
                .width(Size::fill())
                .spacing(Dim::SPACE2)
                .map(error, |el, error| {
                    el.child(
                        rect()
                            .width(Size::fill())
                            .padding(Dim::SPACE2)
                            .corner_radius(Dim::RADIUS)
                            .background(look().0.error_soft)
                            .child(txt(error, look().1.font_sm(), look().0.error, 500)),
                    )
                })
                .children(tree.into_iter().map(|node| {
                    NodeView {
                        node,
                        session: session.clone(),
                        rev,
                    }
                    .into_element()
                })),
        )
    }
}

#[derive(Clone)]
struct NodeView {
    node: ResolvedTree,
    session: Rc<RefCell<Session>>,
    rev: State<u64>,
}

impl PartialEq for NodeView {
    fn eq(&self, other: &Self) -> bool {
        self.node == other.node
            && Rc::ptr_eq(&self.session, &other.session)
            && self.rev == other.rev
    }
}

impl Component for NodeView {
    fn render(&self) -> impl IntoElement {
        // Signal values (an input's text, a checkbox) change without the resolved node changing, so
        // every node redraws when the session does.
        let _ = self.rev.read();
        let mut selected = use_state(String::new);
        let mut local = use_state(String::new);
        let node = &self.node;
        let (c, d) = look();
        if !attr_bool(node, "visible", true) {
            return rect().into_element();
        }

        let session = self.session.clone();
        let rev = self.rev;
        let in_row = node.component == HeComponent::Row && !attr_bool(node, "wrap", false);
        let children: Vec<Element> = node
            .children
            .iter()
            .cloned()
            .map(|child| {
                let grows = in_row && grows_in_row(&child);
                let child = NodeView {
                    node: child,
                    session: session.clone(),
                    rev,
                }
                .into_element();
                if grows {
                    // Text and fields share what the buttons leave of the row.
                    rect()
                        .width(Size::flex(1.))
                        .min_width(Size::px(96.))
                        .child(child)
                        .into_element()
                } else {
                    child
                }
            })
            .collect();

        let id = node.id.clone();
        match node.component {
            HeComponent::Column => {
                rect()
                    .vertical()
                    .width(Size::fill())
                    .spacing(gap(node))
                    .cross_align(align(node, "align"))
                    .children(children)
                    .into_element()
            }
            HeComponent::Row => {
                // Fields have a label above them, so a row with one lines its children up on the
                // bottom edge (the field and the button beside it); other rows center theirs.
                let form = node.children.iter().any(|c| {
                    matches!(
                        c.component,
                        HeComponent::Input | HeComponent::Select | HeComponent::Slider
                    )
                });
                let cross = match attr_str(node, "align").as_str() {
                    "center" if form => Alignment::End,
                    _ => align(node, "align"),
                };
                let content = if attr_bool(node, "wrap", false) {
                    Content::wrap_spacing(gap(node))
                } else {
                    Content::flex()
                };
                rect()
                    .horizontal()
                    .width(Size::fill())
                    .content(content)
                    .spacing(gap(node))
                    .main_align(justify(node))
                    .cross_align(cross)
                    .children(children)
                    .into_element()
            }
            HeComponent::Card => card()
                .child(
                    card_body(Dim::SPACE2)
                        .maybe(!attr_str(node, "title").is_empty(), |el| {
                            el.child(txt(
                                attr_str(node, "title"),
                                d.font_lg(),
                                c.text_strong,
                                700,
                            ))
                        })
                        .children(children),
                )
                .into_element(),
            HeComponent::List => {
                let empty = attr_str(node, "empty");
                rect()
                    .vertical()
                    .width(Size::fill())
                    .spacing(Dim::SPACE1)
                    .maybe(children.is_empty() && !empty.is_empty(), |el| {
                        el.child(hint(empty).into_element())
                    })
                    .children(children)
                    .into_element()
            }
            HeComponent::Tabs => {
                let tabs: Vec<&ResolvedTree> = node
                    .children
                    .iter()
                    .filter(|t| t.component == HeComponent::Tab && attr_bool(t, "visible", true))
                    .collect();
                if tabs.is_empty() {
                    return rect().into_element();
                }
                let bind = attr_str(node, "bind");
                let bound = if bind.is_empty() {
                    None
                } else {
                    session
                        .borrow()
                        .value(&bind)
                        .and_then(Json::as_str)
                        .map(str::to_owned)
                };
                let current = bound
                    .filter(|id| tabs.iter().any(|t| &t.id == id))
                    .or_else(|| {
                        let local = selected.read().clone();
                        tabs.iter().any(|t| t.id == local).then_some(local)
                    })
                    .unwrap_or_else(|| tabs[0].id.clone());

                let mut bar = rect()
                    .horizontal()
                    .width(Size::fill())
                    .content(Content::wrap_spacing(Dim::SPACE1))
                    .spacing(Dim::SPACE1);
                for tab in &tabs {
                    let label = attr_str(tab, "label");
                    let is_current = tab.id == current;
                    let pickup = tab.id.clone();
                    let s = session.clone();
                    let r = rev;
                    let bind = bind.clone();
                    let tabs_id = id.clone();
                    let on = move |_| {
                        if !bind.is_empty() {
                            if let Ok(mut s) = s.try_borrow_mut() {
                                s.write(&bind, Json::String(pickup.clone()));
                                s.act(&tabs_id, "onChange");
                            }
                        }
                        selected.set(pickup.clone());
                        bump(r);
                    };
                    bar = bar.child(if is_current {
                        Btn::primary(label).on_press(on).into_element()
                    } else {
                        Btn::secondary(label).on_press(on).into_element()
                    });
                }
                let body = tabs
                    .iter()
                    .find(|t| t.id == current)
                    .map(|t| {
                        NodeView {
                            node: (*t).clone(),
                            session: session.clone(),
                            rev,
                        }
                        .into_element()
                    })
                    .unwrap_or_else(|| rect().into_element());
                rect()
                    .vertical()
                    .width(Size::fill())
                    .spacing(Dim::SPACE2)
                    .child(bar)
                    .child(body)
                    .into_element()
            }
            HeComponent::Tab => rect()
                .vertical()
                .width(Size::fill())
                .spacing(gap(node))
                .children(children)
                .into_element(),
            HeComponent::Dialog => {
                if !attr_bool(node, "open", true) {
                    return rect().into_element();
                }
                let dismiss = node
                    .attrs
                    .get("onDismiss")
                    .is_some_and(|handler| !handler.is_null());
                let s = session.clone();
                let r = rev;
                let dismiss_id = id.clone();
                rect()
                    .layer(Layer::Overlay)
                    .position(Position::new_global().top(0.).left(0.))
                    .width(Size::window_percent(100.))
                    .height(Size::window_percent(100.))
                    .center()
                    .background(Color::BLACK.with_a(120))
                    .child(
                        card().width(Size::px(440.)).child(
                            card_body(Dim::SPACE3)
                                .maybe(!attr_str(node, "title").is_empty(), |el| {
                                    el.child(txt(
                                        attr_str(node, "title"),
                                        d.font_lg(),
                                        c.text_strong,
                                        700,
                                    ))
                                })
                                .children(children)
                                .maybe(dismiss, |el| {
                                    el.child(
                                        actions().child(
                                            Btn::secondary("Close")
                                                .on_press(move |_| {
                                                    if let Ok(mut s) = s.try_borrow_mut() {
                                                        s.act(&dismiss_id, "onDismiss");
                                                    }
                                                    bump(r);
                                                })
                                                .into_element(),
                                        ),
                                    )
                                }),
                        ),
                    )
                    .into_element()
            }
            HeComponent::Text => {
                let (size, weight, color) = match attr_str(node, "variant").as_str() {
                    "h1" => (d.font_xl(), 700, c.text_strong),
                    "h2" => (d.font_lg(), 700, c.text_strong),
                    "h3" => (d.font_md(), 700, c.text_strong),
                    "caption" => (d.font_sm(), 400, c.text_muted),
                    "code" => (d.font_sm(), 500, c.text_strong),
                    _ => (d.font_md(), 400, c.text_strong),
                };
                txt(attr_str(node, "text"), size, color, weight)
                    .width(Size::fill())
                    .into_element()
            }
            HeComponent::Notice => {
                let (bg, fg) = tone_colors(attr_str(node, "variant").as_str());
                rect()
                    .width(Size::fill())
                    .corner_radius(Dim::RADIUS)
                    .background(bg)
                    .padding(Dim::SPACE2)
                    .spacing(2.)
                    .maybe(!attr_str(node, "title").is_empty(), |el| {
                        el.child(txt(attr_str(node, "title"), d.font_md(), fg, 650))
                    })
                    .child(txt(attr_str(node, "text"), d.font_sm(), fg, 400))
                    .into_element()
            }
            HeComponent::Badge => badge(
                attr_str(node, "text"),
                tone(attr_str(node, "variant").as_str()),
            )
            .into_element(),
            HeComponent::Image => {
                let alt = attr_str(node, "alt");
                let src = attr_str(node, "src");
                if let Some(bytes) = image_bytes(&src) {
                    let (width, height) = match attr_str(node, "variant").as_str() {
                        "avatar" => (Size::px(48.), Size::px(48.)),
                        "thumbnail" => (Size::px(120.), Size::px(120.)),
                        "full" => (Size::fill(), Size::px(300.)),
                        _ => (Size::fill(), Size::px(160.)),
                    };
                    return ImageViewer::new(bytes::Bytes::from(bytes))
                        .width(width)
                        .height(height)
                        .into_element();
                }
                rect()
                    .width(Size::fill())
                    .height(Size::px(120.))
                    .corner_radius(Dim::RADIUS)
                    .background(c.inset_bg)
                    .center()
                    .child(txt(
                        if alt.is_empty() { src } else { alt },
                        d.font_sm(),
                        c.text_muted,
                        400,
                    ))
                    .into_element()
            }
            HeComponent::Divider => divider().into_element(),
            HeComponent::Progress => {
                let value = node.attrs.get("value").and_then(Json::as_f64).unwrap_or(0.);
                rect()
                    .vertical()
                    .width(Size::fill())
                    .spacing(Dim::SPACE1)
                    .maybe(!attr_str(node, "label").is_empty(), |el| {
                        el.child(txt(attr_str(node, "label"), d.font_sm(), c.text_muted, 400))
                    })
                    .child(progress_bar(value as f32))
                    .into_element()
            }
            HeComponent::Button => {
                let label = attr_str(node, "label");
                let variant = attr_str(node, "variant");
                let enabled = !attr_bool(node, "disabled", false);
                let s = session.clone();
                let r = rev;
                let button_id = id.clone();
                let on = move |_| {
                    if let Ok(mut s) = s.try_borrow_mut() {
                        s.act(&button_id, "onPress");
                    }
                    bump(r);
                };
                match variant.as_str() {
                    "primary" | "success" => Btn::primary(label)
                        .enabled(enabled)
                        .on_press(on)
                        .into_element(),
                    "failure" => Btn::danger(label)
                        .enabled(enabled)
                        .on_press(on)
                        .into_element(),
                    "plain" => Btn::ghost(label)
                        .enabled(enabled)
                        .on_press(on)
                        .into_element(),
                    _ => Btn::secondary(label)
                        .enabled(enabled)
                        .on_press(on)
                        .into_element(),
                }
            }
            HeComponent::Input => {
                let bind = attr_str(node, "bind");
                let path = (!bind.is_empty()).then_some(bind);
                let number = attr_str(node, "variant") == "number";
                let multiline = attr_str(node, "variant") == "multiline";
                let bound = path
                    .as_deref()
                    .and_then(|path| session.borrow().value(path).cloned());
                // A number being typed ("1.", "-", "1e") is not yet the number it will become, so the
                // raw text stays while it still means the bound value.
                let value = match bound {
                    Some(value) => {
                        let typed = local.read().clone();
                        if number && typed_number(&typed, &value) {
                            typed
                        } else {
                            json_text(&value)
                        }
                    }
                    None => local.read().clone(),
                };
                let placeholder = attr_str(node, "placeholder");
                let enabled = !attr_bool(node, "disabled", false);
                let input_id = id.clone();

                let s = session.clone();
                let r = rev;
                let p = path.clone();
                let change_id = input_id.clone();
                let on_change = move |text: String| {
                    if number {
                        local.set(text.clone());
                    }
                    if let Some(path) = &p {
                        let value = if number {
                            number_json(&text)
                        } else {
                            Json::String(text.clone())
                        };
                        if let Ok(mut s) = s.try_borrow_mut() {
                            s.write(path, value);
                            s.act(&change_id, "onChange");
                        }
                    } else {
                        local.set(text.clone());
                    }
                    bump(r);
                };
                let s = session.clone();
                let r = rev;
                let p = path;
                let submit_id = input_id.clone();
                let on_submit = move |text: String| {
                    if let Some(path) = &p {
                        let value = if number {
                            number_json(&text)
                        } else {
                            Json::String(text)
                        };
                        if let Ok(mut s) = s.try_borrow_mut() {
                            s.write(path, value);
                            s.act(&submit_id, "onSubmit");
                        }
                    }
                    bump(r);
                };
                let label = attr_str(node, "label");
                let error = attr_str(node, "error");
                let mut field = rect().vertical().width(Size::fill()).spacing(2.);
                if !label.is_empty() {
                    field = field.child(txt(label, d.font_sm(), c.text_muted, 500));
                }
                field = field.child(
                    Input::new(WritableSetter::new(value, EventHandler::new(on_change)))
                        .placeholder(placeholder)
                        .multiline(multiline)
                        .enabled(enabled)
                        .on_submit(on_submit)
                        .width(Size::fill()),
                );
                if !error.is_empty() {
                    field = field.child(txt(error, d.font_xs(), c.error, 400));
                }
                field.into_element()
            }
            HeComponent::Checkbox => {
                let bind = attr_str(node, "bind");
                let value = session
                    .borrow()
                    .value(&bind)
                    .and_then(Json::as_bool)
                    .unwrap_or(false);
                let label = attr_str(node, "label");
                let enabled = !attr_bool(node, "disabled", false);
                let s = session.clone();
                let r = rev;
                let box_id = id.clone();
                let bind = bind;
                let on = move |_| {
                    if !bind.is_empty() {
                        if let Ok(mut s) = s.try_borrow_mut() {
                            s.write(&bind, Json::Bool(!value));
                            s.act(&box_id, "onChange");
                        }
                    }
                    bump(r);
                };
                rect()
                    .horizontal()
                    .spacing(Dim::SPACE2)
                    .cross_align(Alignment::Center)
                    .maybe(enabled, |el| el.on_pointer_down(on))
                    .child(Checkbox::new().selected(value))
                    .maybe(!label.is_empty(), |el| {
                        el.child(txt(label, d.font_md(), c.text_strong, 400))
                    })
                    .into_element()
            }
            HeComponent::Select => {
                let bind = attr_str(node, "bind");
                let options = select_options(node);
                let current = session
                    .borrow()
                    .value(&bind)
                    .and_then(Json::as_str)
                    .map(str::to_owned)
                    .unwrap_or_default();
                let control: Element = if attr_str(node, "variant") == "dropdown" {
                    let selected = options
                        .iter()
                        .find(|(value, _)| *value == current)
                        .map(|(_, label)| label.clone())
                        .unwrap_or_default();
                    Select::new()
                        .selected_item(txt(selected, d.font_md(), c.text_strong, 400))
                        .children(options.into_iter().map(|(value, label)| {
                            let s = session.clone();
                            let r = rev;
                            let bind = bind.clone();
                            let select_id = id.clone();
                            MenuItem::new()
                                .on_press(move |_| {
                                    if let Ok(mut s) = s.try_borrow_mut() {
                                        s.write(&bind, Json::String(value.clone()));
                                        s.act(&select_id, "onChange");
                                    }
                                    bump(r);
                                })
                                .child(txt(label, d.font_md(), c.text_strong, 400))
                                .into_element()
                        }))
                        .into_element()
                } else {
                    let mut row = rect()
                        .horizontal()
                        .width(Size::fill())
                        .content(Content::wrap_spacing(Dim::SPACE1))
                        .spacing(Dim::SPACE1);
                    for (value, label) in options {
                        let is_current = value == current;
                        let s = session.clone();
                        let r = rev;
                        let bind = bind.clone();
                        let select_id = id.clone();
                        let on = move |_| {
                            if let Ok(mut s) = s.try_borrow_mut() {
                                s.write(&bind, Json::String(value.clone()));
                                s.act(&select_id, "onChange");
                            }
                            bump(r);
                        };
                        row = row.child(if is_current {
                            Btn::primary(label).on_press(on).into_element()
                        } else {
                            Btn::secondary(label).on_press(on).into_element()
                        });
                    }
                    row.into_element()
                };
                labeled(node, control)
            }
            HeComponent::Slider => {
                let bind = attr_str(node, "bind");
                let min = attr_num(node, "min").unwrap_or(0.);
                let max = attr_num(node, "max").unwrap_or(100.);
                let step = attr_num(node, "step").unwrap_or(1.);
                let value = session
                    .borrow()
                    .value(&bind)
                    .and_then(Json::as_f64)
                    .unwrap_or(min);
                let percent = if max > min {
                    ((value - min) / (max - min) * 100.).clamp(0., 100.)
                } else {
                    0.
                };
                let enabled = !attr_bool(node, "disabled", false);
                let s = session.clone();
                let r = rev;
                let slider_id = id.clone();
                let step_percent = if max > min {
                    step / (max - min) * 100.
                } else {
                    1.
                };
                let control: Element = Slider::new(move |v| {
                    let actual = min + (max - min) * v / 100.;
                    if let Ok(mut s) = s.try_borrow_mut() {
                        s.write(&bind, json!(actual));
                        s.act(&slider_id, "onChange");
                    }
                    bump(r);
                })
                .value(percent)
                .enabled(enabled)
                .step(step_percent)
                .size(Size::fill())
                .into_element();
                labeled(node, control)
            }
        }
    }
}

/// Whether a child of a row takes the row's spare width instead of its own size.
fn grows_in_row(node: &ResolvedTree) -> bool {
    attr_bool(node, "visible", true)
        && matches!(
            node.component,
            HeComponent::Text
                | HeComponent::Input
                | HeComponent::Select
                | HeComponent::Slider
                | HeComponent::Progress
                | HeComponent::Checkbox
                | HeComponent::Column
                | HeComponent::Card
                | HeComponent::List
        )
}

fn image_bytes(src: &str) -> Option<Vec<u8>> {
    if let Some(rest) = src.strip_prefix("data:") {
        let (meta, data) = rest.split_once(',')?;
        if !meta.ends_with(";base64") {
            return None;
        }
        return base64::engine::general_purpose::STANDARD.decode(data).ok();
    }
    let path = src.strip_prefix("file://").unwrap_or(src);
    if path.starts_with('/') {
        return std::fs::read(path).ok();
    }
    None
}

fn labeled(node: &ResolvedTree, control: Element) -> Element {
    let (c, d) = look();
    let label = attr_str(node, "label");
    let error = attr_str(node, "error");
    let mut field = rect().vertical().width(Size::fill()).spacing(2.);
    if !label.is_empty() {
        field = field.child(txt(label, d.font_sm(), c.text_muted, 500));
    }
    field = field.child(control);
    if !error.is_empty() {
        field = field.child(txt(error, d.font_xs(), c.error, 400));
    }
    field.into_element()
}

fn bump(mut rev: State<u64>) {
    let next = *rev.read() + 1;
    rev.set(next);
}

fn attr<'a>(node: &'a ResolvedTree, name: &str) -> Option<&'a Json> {
    node.attrs.get(name)
}

fn attr_str(node: &ResolvedTree, name: &str) -> String {
    attr(node, name)
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_owned()
}

fn attr_bool(node: &ResolvedTree, name: &str, default: bool) -> bool {
    attr(node, name).and_then(Json::as_bool).unwrap_or(default)
}

fn attr_num(node: &ResolvedTree, name: &str) -> Option<f64> {
    attr(node, name).and_then(Json::as_f64)
}

fn gap(node: &ResolvedTree) -> f32 {
    match attr_str(node, "gap").as_str() {
        "none" => 0.,
        "small" => Dim::SPACE1,
        "large" => Dim::SPACE3,
        _ => Dim::SPACE2,
    }
}

fn align(node: &ResolvedTree, name: &str) -> Alignment {
    match attr_str(node, name).as_str() {
        "center" => Alignment::Center,
        "end" => Alignment::End,
        _ => Alignment::Start,
    }
}

fn justify(node: &ResolvedTree) -> Alignment {
    match attr_str(node, "justify").as_str() {
        "center" => Alignment::Center,
        "end" => Alignment::End,
        "between" => Alignment::SpaceBetween,
        _ => Alignment::Start,
    }
}

fn tone(variant: &str) -> Tone {
    match variant {
        "success" => Tone::Ok,
        "warning" => Tone::Warn,
        "failure" => Tone::Error,
        "info" => Tone::Active,
        _ => Tone::Muted,
    }
}

fn tone_colors(variant: &str) -> (Color, Color) {
    let (c, _) = look();
    match variant {
        "success" => (c.success_soft, c.success),
        "warning" => (c.warn_soft, c.warn),
        "failure" => (c.error_soft, c.error),
        _ => (c.info_soft, c.info),
    }
}

fn select_options(node: &ResolvedTree) -> Vec<(String, String)> {
    attr(node, "options")
        .and_then(Json::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(|option| {
                    Some((
                        option.get("value")?.as_str()?.to_owned(),
                        option.get("label")?.as_str()?.to_owned(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// What a number input holds for the text typed in it: a number, or null when it is not one.
fn number_json(text: &str) -> Json {
    match text.trim().parse::<f64>() {
        Ok(n) if n.is_finite() => match text.trim().parse::<i64>() {
            Ok(i) => json!(i),
            Err(_) => json!(n),
        },
        _ => Json::Null,
    }
}

/// Whether `typed` is still a way of writing the bound `value`.
fn typed_number(typed: &str, value: &Json) -> bool {
    match (number_json(typed).as_f64(), value.as_f64()) {
        (Some(typed), Some(value)) => typed == value,
        (None, None) => number_json(typed).is_null() && value.is_null(),
        _ => false,
    }
}

fn json_text(value: &Json) -> String {
    match value {
        Json::String(s) => s.clone(),
        Json::Number(n) => n.to_string(),
        Json::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

fn json_to_cbor(value: Json) -> Cbor {
    match value {
        Json::Null => Cbor::Null,
        Json::Bool(b) => Cbor::Bool(b),
        Json::Number(n) => {
            if let Some(i) = n.as_i64() {
                Cbor::Integer(i.into())
            } else if let Some(u) = n.as_u64() {
                Cbor::Integer(u.into())
            } else {
                Cbor::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        Json::String(s) => Cbor::Text(s),
        Json::Array(a) => Cbor::Array(a.into_iter().map(json_to_cbor).collect()),
        Json::Object(o) => Cbor::Map(
            o.into_iter()
                .map(|(k, v)| (Cbor::Text(k), json_to_cbor(v)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{Look, Palette};
    use flipflop::groupdoc::GroupDoc;
    use freya::radio::use_init_radio_station;
    use freya_testing::prelude::*;
    use std::sync::Mutex;

    struct Shared(Mutex<GroupDoc>);

    impl DocHost for Shared {
        fn with_doc(&self, f: &mut dyn FnMut(&mut GroupDoc)) {
            f(&mut self.0.lock().unwrap());
        }
    }

    #[derive(Clone)]
    struct Harness {
        session: Rc<RefCell<Session>>,
    }

    impl PartialEq for Harness {
        fn eq(&self, other: &Self) -> bool {
            Rc::ptr_eq(&self.session, &other.session)
        }
    }

    impl Component for Harness {
        fn render(&self) -> impl IntoElement {
            let palette = Palette::new(true);
            let dim = Dim {
                touch: false,
                narrow: false,
            };
            let look = Look { palette, dim };
            let mut look_state = use_provide_context(|| State::create(look));
            look_state.set_if_modified(look);
            let _ = use_init_theme(|| look.palette.freya_theme());
            let _ = use_init_radio_station::<Model, Channel>(Model::default);
            let rev = use_state(|| 0u64);
            rect()
                .expanded()
                .background(look.palette.window_bg)
                .padding(Dim::SPACE3)
                .child(HyperView {
                    session: self.session.clone(),
                    rev,
                })
                .into_element()
        }
    }

    fn texts(runner: &TestingRunner) -> Vec<String> {
        runner.find_many(|_, element| {
            Label::try_downcast(element).map(|label| label.text.to_string())
        })
    }

    fn click(runner: &mut TestingRunner, text: &str) {
        let at = runner
            .find_many(|node, element| {
                let label = Label::try_downcast(element)?;
                (label.text == text).then(|| node.layout().area.center())
            })
            .first()
            .copied();
        if let Some(at) = at {
            runner.click_cursor((at.x as f64, at.y as f64));
        }
    }

    #[test]
    fn numbers_keep_what_is_typed() {
        assert_eq!(number_json("12"), json!(12));
        assert_eq!(number_json("1.5"), json!(1.5));
        assert_eq!(number_json("-"), Json::Null);
        assert_eq!(number_json(""), Json::Null);
        assert_eq!(number_json("NaN"), Json::Null);
        assert!(typed_number("1.", &json!(1)));
        assert!(!typed_number("2", &json!(1)));
        assert!(typed_number("-", &Json::Null));
    }

    fn find(nodes: &[ResolvedTree], component: HeComponent, out: &mut Vec<String>) {
        for node in nodes {
            if node.component == component {
                out.push(node.id.clone());
            }
            find(&node.children, component, out);
        }
    }

    /// The ids of the elements of one kind, in order.
    fn ids(session: &Session, component: HeComponent) -> Vec<String> {
        let mut out = Vec::new();
        find(&session.tree(), component, &mut out);
        out
    }

    fn press(session: &mut Session, id: &str) {
        session.act(id, "onPress");
    }

    /// Type into the input `id`, which is bound to `path`.
    fn fill(session: &mut Session, id: &str, path: &str, text: &str) {
        session.write(path, json!(text));
        session.act(id, "onChange");
    }

    fn choose(session: &mut Session, id: &str, path: &str, value: Json) {
        session.write(path, value);
        session.act(id, "onChange");
    }

    /// Some data for the screenshots, entered the way a person would.
    fn seed(session: &mut Session, app: &str) {
        match app {
            "checklist" => {
                for item in ["Milk", "Bread", "Coffee beans"] {
                    fill(session, "add-text", "form.item", item);
                    press(session, "add-go");
                }
                let first = ids(session, HeComponent::Checkbox).remove(0);
                choose(session, &first, &format!("sync.{}", &first[..first.len() - 5].replacen("i-", "done.", 1)), json!(true));
            }
            "poll" => {
                fill(session, "new-q", "form.question", "Where shall we eat on Friday?");
                fill(session, "new-o", "form.options", "Pizza\nSushi\nTacos at the market");
                press(session, "new-go");
                fill(session, "new-q", "form.question", "Which days work for the trip?");
                fill(session, "new-o", "form.options", "Saturday\nSunday");
                choose(session, "new-multi", "form.multi", json!(true));
                press(session, "new-go");
                let votes: Vec<String> = ids(session, HeComponent::Button)
                    .into_iter()
                    .filter(|id| id.ends_with("-vote"))
                    .collect();
                // The newest poll is first.
                press(session, &votes[0]);
                press(session, &votes[1]);
                press(session, &votes[3]);
                // A mistake is explained.
                fill(session, "new-q", "form.question", "Only one option");
                fill(session, "new-o", "form.options", "Just this");
                press(session, "new-go");
            }
            "expenses" => {
                for (what, amount, payer) in [
                    ("Groceries", 42.5, "me"),
                    ("Taxi home", 18.0, "bob"),
                    ("Pizza night", 31.9, "carol"),
                ] {
                    fill(session, "new-what", "form.what", what);
                    choose(session, "new-amount", "form.amount", json!(amount));
                    choose(session, "new-payer", "form.payer", json!(payer));
                    press(session, "new-go");
                }
                let pay = ids(session, HeComponent::Button).into_iter().find(|id| id.ends_with("-go") && id.starts_with("pay")).unwrap();
                press(session, &pay);
                // A mistake is explained.
                fill(session, "new-what", "form.what", "Free lunch");
                press(session, "new-go");
            }
            "availability" => {
                fill(session, "new-title", "form.title", "Board game night");
                fill(session, "new-note", "form.note", "At my place, food provided");
                press(session, "new-go");
                let go = ids(session, HeComponent::Button)
                    .into_iter()
                    .find(|id| id.ends_with("-add-go"))
                    .unwrap();
                let plan = go.strip_suffix("-add-go").unwrap().to_owned();
                let hash = plan.strip_prefix("a-").unwrap().to_owned();
                for (day, month, hour) in [("20", "11", "19"), ("21", "11", "18"), ("4", "3", "10")] {
                    let path = format!("form.slot.{hash}");
                    choose(session, &format!("{plan}-add-day"), &format!("{path}.day"), json!(day));
                    choose(session, &format!("{plan}-add-month"), &format!("{path}.month"), json!(month));
                    choose(session, &format!("{plan}-add-hour"), &format!("{path}.hour"), json!(hour));
                    press(session, &go);
                }
                let first = ids(session, HeComponent::Select)
                    .into_iter()
                    .find(|id| id.ends_with("-ans"))
                    .unwrap();
                let (plan, slot) = hash_part(&first);
                choose(session, &first, &format!("sync.v.{plan}.{slot}"), json!("yes"));
                // A time that is already there is explained.
                press(session, &go);
            }
            "calendar" => {
                for (title, quick, hour, length, place) in [
                    ("Dinner at Ana's", "Tomorrow", "19", "120", "Rua Augusta 12"),
                    ("Standup", "Today", "23", "30", ""),
                    ("Trip to the lake", "Tomorrow", "09", "day", "Meet at the station"),
                ] {
                    fill(session, "new-title", "form.title", title);
                    press(session, &format!("new-quick-{quick}"));
                    choose(session, "new-when-hour", "form.when.hour", json!(hour));
                    choose(session, "new-length", "form.length", json!(length));
                    fill(session, "new-place", "form.place", place);
                    press(session, "new-add");
                }
                fill(session, "new-title", "form.title", "Planning");
                choose(session, "new-when-year", "form.when.year", json!("2028"));
                choose(session, "new-when-month", "form.when.month", json!("1"));
                choose(session, "new-when-day", "form.when.day", json!("2"));
                press(session, "new-add");
                // A mistake is explained, not swallowed.
                fill(session, "new-title", "form.title", "Oops");
                choose(session, "new-when-month", "form.when.month", json!("2"));
                choose(session, "new-when-day", "form.when.day", json!("31"));
                press(session, "new-add");
            }
            _ => {}
        }
    }

    /// The plan and slot hashes of a slot's select id `a-<plan>-s<slot>-ans`.
    fn hash_part(id: &str) -> (String, String) {
        let rest = id.strip_prefix("a-").unwrap().strip_suffix("-ans").unwrap();
        let (plan, slot) = rest.split_once("-s").unwrap();
        (plan.to_owned(), slot.to_owned())
    }

    /// `MINIAPP_SHOTS=<dir> cargo test -p flipflop-app shots` draws each miniapp, with some data in
    /// it, to PNGs (`<app>-<width>.png`) for a look at the layout.
    #[test]
    fn shots() {
        let Some(dir) = std::env::var_os("MINIAPP_SHOTS") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let only = std::env::var("MINIAPP_ONLY").ok();
        for app in ["checklist", "expenses", "poll", "availability", "calendar"] {
            if only.as_deref().is_some_and(|only| only != app) {
                continue;
            }
            let path = format!(
                "{}/../examples/miniapps/dist/{app}.wasm",
                env!("CARGO_MANIFEST_DIR")
            );
            let wasm = std::fs::read(path).expect("run examples/miniapps/build.sh first");
            for width in [900., 380.] {
                let mut doc = GroupDoc::new();
                for (peer, name) in [("me", "Ana"), ("bob", "Bo"), ("carol", "Cy")] {
                    doc.join(peer, name);
                }
                let host = Arc::new(Shared(Mutex::new(doc)));
                let mut session = Session::standalone(app, &wasm, "me", host).unwrap();
                seed(&mut session, app);
                let session = Rc::new(RefCell::new(session));
                let (mut runner, ()) = TestingRunner::new(
                    {
                        let session = session.clone();
                        move || {
                            Harness {
                                session: session.clone(),
                            }
                            .into_element()
                        }
                    },
                    (width, 900.).into(),
                    |_| {},
                    1.0,
                );
                runner.sync_and_update();
                runner.render_to_file(dir.join(format!("{app}-{width}.png")));
            }
        }
    }

    #[test]
    fn typing_in_an_input_edits_it() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../examples/miniapps/dist/checklist.wasm"
        );
        let Ok(wasm) = std::fs::read(path) else {
            eprintln!("skipping: run examples/miniapps/build.sh first");
            return;
        };
        let host = Arc::new(Shared(Mutex::new(GroupDoc::new())));
        let session = Session::standalone("checklist", &wasm, "me", host).expect("the app starts");
        let session = Rc::new(RefCell::new(session));
        let (mut runner, ()) = TestingRunner::new(
            {
                let session = session.clone();
                move || {
                    Harness {
                        session: session.clone(),
                    }
                    .into_element()
                }
            },
            (700., 700.).into(),
            |_| {},
            1.0,
        );
        runner.sync_and_update();
        // The input is right under its label.
        let label = runner
            .find_many(|node, element| {
                let label = Label::try_downcast(element)?;
                (label.text == "Add an item").then(|| node.layout().area.center())
            })
            .first()
            .copied()
            .expect("the label");
        runner.click_cursor((label.x as f64, label.y as f64 + 24.));
        runner.write_text("Milk");
        runner.write_text("y");
        assert_eq!(
            session.borrow().value("form.item").and_then(Json::as_str),
            Some("Milky")
        );
        click(&mut runner, "Add");
        runner.poll(
            std::time::Duration::from_millis(16),
            std::time::Duration::from_millis(50),
        );
        runner.sync_and_update();
        let after = texts(&runner);
        assert!(after.iter().any(|t| t == "Milky"), "{after:?}");
        // The form was cleared by the app.
        assert_eq!(
            session.borrow().value("form.item").and_then(Json::as_str),
            Some("")
        );
    }

    #[test]
    fn the_checklist_asks_for_its_list_name() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../examples/miniapps/dist/checklist.wasm"
        );
        let Ok(wasm) = std::fs::read(path) else {
            eprintln!("skipping: run examples/miniapps/build.sh first");
            return;
        };
        let form = Runtime::default().params(&wasm).expect("it describes itself");
        assert_eq!(form.name.default, "Checklist");
        let [list] = form.params.as_slice() else {
            panic!("{form:?}");
        };
        assert_eq!(
            (list.key.as_str(), list.default.as_str(), list.placeholder.as_str()),
            ("list", "", "Groceries")
        );
    }

    #[test]
    fn draws_and_acts() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../examples/miniapps/dist/checklist.wasm"
        );
        let Ok(wasm) = std::fs::read(path) else {
            eprintln!("skipping: run examples/miniapps/build.sh first");
            return;
        };
        let host = Arc::new(Shared(Mutex::new(GroupDoc::new())));
        let mut session =
            Session::standalone("checklist", &wasm, "me", host).expect("the app starts");
        session.write("form.item", json!("Groceries"));
        let session = Rc::new(RefCell::new(session));

        let (mut runner, ()) = TestingRunner::new(
            {
                let session = session.clone();
                move || {
                    Harness {
                        session: session.clone(),
                    }
                    .into_element()
                }
            },
            (700., 700.).into(),
            |_| {},
            1.0,
        );
        runner.sync_and_update();

        let before = texts(&runner);
        assert!(before.iter().any(|text| text == "Checklist"), "{before:?}");
        assert!(before.iter().any(|text| text == "Nothing here yet."), "{before:?}");

        click(&mut runner, "Add");
        runner.poll(
            std::time::Duration::from_millis(16),
            std::time::Duration::from_millis(50),
        );
        runner.sync_and_update();

        let after = texts(&runner);
        assert!(after.iter().any(|text| text == "Groceries"), "{after:?}");
        assert!(!after.iter().any(|text| text == "Nothing here yet."), "{after:?}");
    }
}
