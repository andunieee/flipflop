//! A Freya renderer for hypereffect, over `hypereffect::client::Client`: the client keeps the
//! element tree, applies signals, evaluates CEL and resolves every attribute; this module draws the
//! resolved tree with the app's own widgets and sends actions back to the wasm app.

use std::{cell::RefCell, rc::Rc, sync::Arc};

use ciborium::Value as Cbor;
use flipflop::{
    apps::{App as WasmApp, DocHost, Runtime},
    core::Core,
    groupdoc::GroupDoc,
};
use freya::{prelude::*, radio::use_radio};
use freya_hypereffect::{
    ButtonProps, ButtonVariant, CardProps, CheckboxProps, HyperHost, InputProps, ProgressProps,
    SharedHost, SharedSkin, Skin, Style,
};
use hypereffect::{
    Value as Json,
    client::{Client, ResolvedTree},
    message::RendererMessage,
};

use crate::{
    backend::Backend,
    model::{Channel, Model},
    theme::{Dim, look},
    widgets::{Btn, area, card, card_body, field, progress_bar, txt},
};

/// One app session: the wasm app, the renderer core, and the group document it writes to.
pub struct Session {
    backend: Option<Backend>,
    gid: String,
    app: WasmApp,
    app_id: String,
    client: Client,
    /// The name the app last gave its tab.
    named: Option<String>,
    /// Counts what the app emitted, so views redraw when a signal changes without a local action.
    pub frame: u64,
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
            app_id: app_id.to_owned(),
            client,
            named: None,
            frame: 0,
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
            self.frame += 1;
        }
        self.rename_tab();
        self.report();
    }

    /// The app named its tab (a `metadata` message): tell the group, once per change.
    fn rename_tab(&mut self) {
        let name = self.client.metadata().name.clone();
        if name.is_none() || name == self.named {
            return;
        }
        self.named = name.clone();
        let (Some(backend), Some(name)) = (&self.backend, name) else {
            return;
        };
        let (gid, id) = (self.gid.clone(), self.app_id.clone());
        backend.run(move |core| core.set_app_name(&gid, &id, &name));
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

impl HyperHost for Session {
    fn tree(&self) -> Vec<ResolvedTree> {
        Session::tree(self)
    }

    fn value(&self, path: &str) -> Option<Json> {
        Session::value(self, path).cloned()
    }

    fn write(&mut self, path: &str, value: Json) {
        Session::write(self, path, value)
    }

    fn act(&mut self, id: &str, event: &str) {
        Session::act(self, id, event)
    }

    fn revision(&self) -> u64 {
        self.frame
    }

    fn error(&self) -> Option<String> {
        self.error.clone()
    }
}

/// The controls of an app, drawn with the app's own widgets so they look like the rest of it.
struct AppSkin;

impl Skin for AppSkin {
    fn button(&self, props: ButtonProps) -> Element {
        let on_press = props.on_press;
        match props.variant {
            ButtonVariant::Primary => Btn::primary(props.label),
            ButtonVariant::Danger => Btn::danger(props.label),
            ButtonVariant::Plain => Btn::ghost(props.label),
            ButtonVariant::Secondary => Btn::secondary(props.label),
        }
        .enabled(props.enabled)
        .on_press(move |()| on_press.call(()))
        .into_element()
    }

    fn card(&self, props: CardProps) -> Element {
        let (c, d) = look();
        card()
            .child(
                card_body(Dim::SPACE2)
                    .map(props.title, |el, title| {
                        el.child(txt(title, d.font_lg(), c.text_strong, 700))
                    })
                    .children(props.children),
            )
            .into_element()
    }

    fn input(&self, props: InputProps) -> Element {
        let (on_change, on_submit) = (props.on_change, props.on_submit);
        if props.multiline {
            area(props.value, props.placeholder, move |text| {
                on_change.call(text)
            })
        } else {
            field(props.value, props.placeholder, move |text| {
                on_change.call(text)
            })
        }
        .enabled(props.enabled)
        .on_submit(move |text: String| on_submit.call(text))
        .into_element()
    }

    fn checkbox(&self, props: CheckboxProps) -> Element {
        let (c, d) = look();
        let on_toggle = props.on_toggle;
        rect()
            .horizontal()
            .spacing(Dim::SPACE2)
            .cross_align(Alignment::Center)
            .maybe(props.enabled, |el| {
                el.on_pointer_down(move |_: Event<PointerEventData>| on_toggle.call(()))
            })
            .child(Checkbox::new().selected(props.checked))
            .maybe(!props.label.is_empty(), |el| {
                el.child(txt(props.label, d.font_md(), c.text_strong, 400))
            })
            .into_element()
    }

    fn progress(&self, props: ProgressProps) -> Element {
        let (c, d) = look();
        rect()
            .vertical()
            .width(Size::fill())
            .spacing(Dim::SPACE1)
            .map(props.label, |el, label| {
                el.child(txt(label, d.font_sm(), c.text_muted, 400))
            })
            .child(progress_bar(props.fraction))
            .into_element()
    }
}

/// The app's interface, drawn by `freya-hypereffect`. `rev` is bumped after local actions so
/// Freya redraws; group changes arrive through the model and are pumped here before drawing.
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
        let host: SharedHost = self.session.clone();
        let skin = use_hook(|| -> SharedSkin { Rc::new(AppSkin) }).clone();

        ScrollView::new().expanded().spacing(Dim::SPACE3).child(
            rect().width(Size::fill()).child(
                freya_hypereffect::HyperView::new(host, self.rev)
                    .skin(skin)
                    .style(Style {
                        space_small: Dim::SPACE1,
                        space_medium: Dim::SPACE2,
                        space_large: Dim::SPACE3,
                        radius: Dim::RADIUS,
                    }),
            ),
        )
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
    use hypereffect::{Component as HeComponent, json};
    use std::sync::Mutex;

    struct Shared(Mutex<GroupDoc>);

    impl DocHost for Shared {
        fn with_doc(&self, f: &mut dyn FnMut(&mut GroupDoc)) {
            f(&mut self.0.lock().unwrap());
        }
    }

    thread_local! {
        /// The counter the harness's view redraws on, for tests that change the app from outside.
        static REV: RefCell<Option<State<u64>>> = const { RefCell::new(None) };
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
                phone: false,
                touch: false,
                narrow: false,
            };
            let look = Look { palette, dim };
            let mut look_state = use_provide_context(|| State::create(look));
            look_state.set_if_modified(look);
            let _ = use_init_theme(|| look.palette.freya_theme());
            let _ = use_init_radio_station::<Model, Channel>(Model::default);
            let rev = use_state(|| 0u64);
            REV.with(|slot| *slot.borrow_mut() = Some(rev));
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

    /// The text typed or shown in fields, which Freya draws as paragraphs, not labels.
    fn fields(runner: &TestingRunner) -> Vec<String> {
        runner.find_many(|_, element| {
            let paragraph = Paragraph::try_downcast(element)?;
            Some(
                paragraph
                    .spans
                    .iter()
                    .map(|span| span.text.as_ref())
                    .collect(),
            )
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
                choose(
                    session,
                    &first,
                    &format!(
                        "sync.{}",
                        &first[..first.len() - 5].replacen("i-", "done.", 1)
                    ),
                    json!(true),
                );
            }
            "poll" => {
                fill(
                    session,
                    "new-q",
                    "form.question",
                    "Where shall we eat on Friday?",
                );
                fill(
                    session,
                    "new-o",
                    "form.options",
                    "Pizza\nSushi\nTacos at the market",
                );
                press(session, "new-go");
                fill(
                    session,
                    "new-q",
                    "form.question",
                    "Which days work for the trip?",
                );
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
                let pay = ids(session, HeComponent::Button)
                    .into_iter()
                    .find(|id| id.ends_with("-go") && id.starts_with("pay"))
                    .unwrap();
                press(session, &pay);
                // A mistake is explained.
                fill(session, "new-what", "form.what", "Free lunch");
                press(session, "new-go");
            }
            "availability" => {
                fill(session, "new-title", "form.title", "Board game night");
                fill(
                    session,
                    "new-note",
                    "form.note",
                    "At my place, food provided",
                );
                press(session, "new-go");
                let go = ids(session, HeComponent::Button)
                    .into_iter()
                    .find(|id| id.ends_with("-add-go"))
                    .unwrap();
                let plan = go.strip_suffix("-add-go").unwrap().to_owned();
                let hash = plan.strip_prefix("a-").unwrap().to_owned();
                for (day, month, hour) in [("20", "11", "19"), ("21", "11", "18"), ("4", "3", "10")]
                {
                    let path = format!("form.slot.{hash}");
                    choose(
                        session,
                        &format!("{plan}-add-day"),
                        &format!("{path}.day"),
                        json!(day),
                    );
                    choose(
                        session,
                        &format!("{plan}-add-month"),
                        &format!("{path}.month"),
                        json!(month),
                    );
                    choose(
                        session,
                        &format!("{plan}-add-hour"),
                        &format!("{path}.hour"),
                        json!(hour),
                    );
                    press(session, &go);
                }
                let first = ids(session, HeComponent::Select)
                    .into_iter()
                    .find(|id| id.ends_with("-ans"))
                    .unwrap();
                let (plan, slot) = hash_part(&first);
                choose(
                    session,
                    &first,
                    &format!("sync.v.{plan}.{slot}"),
                    json!("yes"),
                );
                // A time that is already there is explained.
                press(session, &go);
            }
            "calendar" => {
                for (title, quick, hour, length, place) in [
                    ("Dinner at Ana's", "Tomorrow", "19", "120", "Rua Augusta 12"),
                    ("Standup", "Today", "23", "30", ""),
                    (
                        "Trip to the lake",
                        "Tomorrow",
                        "09",
                        "day",
                        "Meet at the station",
                    ),
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
        // The input is on the row of the "+" button, before it.
        let plus = runner
            .find_many(|node, element| {
                let label = Label::try_downcast(element)?;
                (label.text == "+").then(|| node.layout().area.center())
            })
            .first()
            .copied()
            .expect("the button");
        runner.click_cursor((plus.x as f64 - 100., plus.y as f64));
        runner.write_text("Milk");
        runner.write_text("y");
        assert_eq!(
            session.borrow().value("form.item").and_then(Json::as_str),
            Some("Milky")
        );
        click(&mut runner, "+");
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
        let form = Runtime::default()
            .params(&wasm)
            .expect("it describes itself");
        let [list] = form.params.as_slice() else {
            panic!("{form:?}");
        };
        assert_eq!(
            (
                list.key.as_str(),
                list.default.as_str(),
                list.placeholder.as_str()
            ),
            ("list", "", "Groceries")
        );
    }

    /// A tick that did not come from a click here (read from the document at the start, or synced
    /// from another device) shows in the checkbox.
    #[test]
    fn a_tick_from_the_document_shows() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../examples/miniapps/dist/checklist.wasm"
        );
        let Ok(wasm) = std::fs::read(path) else {
            eprintln!("skipping: run examples/miniapps/build.sh first");
            return;
        };
        let host = Arc::new(Shared(Mutex::new(GroupDoc::new())));
        let mut mine = Session::standalone("checklist", &wasm, "me", host.clone()).unwrap();
        fill(&mut mine, "add-text", "form.item", "Milk");
        press(&mut mine, "add-go");
        let item = ids(&mine, HeComponent::Checkbox).remove(0);
        let bind = format!(
            "sync.{}",
            &item[..item.len() - 5].replacen("i-", "done.", 1)
        );
        let ticked = |s: &Session| s.value(&bind).and_then(Json::as_bool);
        assert_eq!(ticked(&mine), Some(false));

        // Another device ticks it: ours hears of it through the document.
        let mut other = Session::standalone("checklist", &wasm, "bob", host.clone()).unwrap();
        choose(&mut other, &item, &bind, json!(true));
        mine.pump();
        assert_eq!(ticked(&mine), Some(true));

        // We tick it ourselves too, then the other one unticks it.
        choose(&mut mine, &item, &bind, json!(true));
        assert_eq!(ticked(&mine), Some(true));
        choose(&mut other, &item, &bind, json!(false));
        mine.pump();
        assert_eq!(ticked(&mine), Some(false));
        choose(&mut other, &item, &bind, json!(true));
        mine.pump();

        // And one that starts later finds it ticked.
        let later = Session::standalone("checklist", &wasm, "carol", host).unwrap();
        assert_eq!(ticked(&later), Some(true));
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
        // Nothing but the way to add an item: no title, no card, no hint.
        assert_eq!(before, ["+"]);

        click(&mut runner, "+");
        runner.poll(
            std::time::Duration::from_millis(16),
            std::time::Duration::from_millis(50),
        );
        runner.sync_and_update();

        let after = texts(&runner);
        assert!(after.iter().any(|text| text == "Groceries"), "{after:?}");
        assert!(after.iter().any(|text| text == "0 of 1 done"), "{after:?}");
    }

    #[test]
    fn a_signal_that_changes_alone_redraws_its_field() {
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
        assert!(!fields(&runner).iter().any(|t| t == "Milk"));

        // The signal changes, the tree does not: only the field showing it has to redraw.
        session.borrow_mut().write("form.item", json!("Milk"));
        let mut rev = REV.with(|slot| slot.borrow().expect("the view rendered"));
        *rev.write() += 1;
        runner.sync_and_update();
        runner.poll(
            std::time::Duration::from_millis(16),
            std::time::Duration::from_millis(50),
        );
        runner.sync_and_update();
        let after = fields(&runner);
        assert!(after.iter().any(|t| t == "Milk"), "{after:?}");
    }
}
