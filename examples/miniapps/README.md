# Experimental miniapps

Five wasm apps for the runtime in `src/apps.rs`, each drawn with the hypereffect components the desktop
app renders (`app/src/hyper.rs`): lists, an expense splitter, polls, a "find a time" planner and a shared
calendar. They are prototypes of the app layer and work in the desktop app's app tabs.

```sh
examples/miniapps/build.sh                 # -> dist/{checklist,expenses,poll,availability,calendar}.wasm
just miniapp checklist actions.json        # one app, one renderer, a list of actions
just miniapp-sync poll a.json b.json       # two peers, then compare their renderers
```

An actions file is a JSON array of `{"name": "...", "payload": {...}, "set": {...}}`. `set` is a merge
patch applied to the renderer's signals first, as a user editing a bound input would. `miniapp` drives
a throwaway document; `miniapp-sync` feeds its first file to peer `me`, syncs, then its second to
`bob`, syncs again, and fails if the two renderers end up different.

```json
[
  {"name": "add_list", "set": {"form": {"list": "Groceries"}}},
  {"name": "add_list", "set": {"form": {"list": "Packing"}}}
]
```

## The flow

The app is a hypereffect **server**; the host is the renderer. The group document is the state.

1. The renderer starts a session (`start` in-process). The app sends `metadata`, the initial
   `signal`s and the element tree: `column`, `card`, `row`, `list`, `text`, `input`, `button`, …
2. The user presses a button. The renderer sends `action {"name":…, "payload":…, "signals":…}`.
3. The app validates it and writes the document (flat scalar keys, below). It renders immediately.
4. The document changes. Every device — including this one — gets a `change` event with the patches,
   runs the app again, which diffs its new tree against the last one and emits only the changed
   `element`s, new elements, `remove`s and any `signal` patch.
5. A device that only synced, without any local interaction, follows steps 4–5 and ends up showing
   the same interface.

Frames are per session and never go into the document: two renderers must not fight over them, and
they are not shared state.

## Why every value is a scalar under its own key

Automerge's `put` of a map or list makes a **new object** under the writer's actor. Two devices doing
that at the same key make two objects, and a merge keeps one — everything written inside the other
is gone. An app runs on every device, so it would do exactly that on its first write.

Hence every app value is a **scalar** (`text`, `int`, `bool`, `null`) under its own flat key:
`l<US>groceries<US>name`, `i<US>groceries<US>milk<US>done`, `b<US>food<US>bob<US>Dinner`. Different
keys merge; the same key is last-writer-wins. Deleting an entity deletes every key with its prefix.
`<US>` is U+001F.

The cost is that the interface is recomputed from all keys after each change. For group-sized data
(tens to hundreds of rows) that is fine; indexes stored as scalars would be the next step.

## Writing an app

An app is a module in `src/apps/` with `NAME`, `form(host)` (the initial input signals), `page(host)`
(the whole interface for the document as it is) and `action(host, event)`, whose `Err` is shown to the
user in a notice. `lib.rs` drives it: it renders at the start, after every `change` and after every
action, and `ui.rs` sends the renderer only what differs from the last frame. Shared pieces:

- `doc.rs`: the app's section read in one request (`Doc`), and the members' names (`People`);
- `act.rs`: an action event: the button's payload, the user's inputs (`form.*`), bound controls (`sync.*`);
- `time.rs`: dates and times shown in the device's own time zone (every event carries `ts` and `tz`); the
  document holds UTC milliseconds;
- `picker.rs`: a date and time picker made of selects (day, month, year, hour, minute), with Today and
  Tomorrow buttons; a date that does not exist (31 Feb) is explained in a notice;
- `ui.rs`: `Page`, element shorthands, the error notice, and a "press again" confirmation for
  destructive buttons.

Controls that mirror the document (a checkbox's state, an RSVP) bind to `sync.*` signals that the app sets
on every page; the user's inputs live under `form.*`, which the app only clears after an action.

## The apps

Each module's doc comment has its exact state keys and actions.

- **checklist** (Lists) — lists of items with checkboxes and a progress bar; add and remove items, clear
  the done ones, delete a list (press twice).
- **expenses** — decimal amounts kept as cents, who paid, who it is split between, balances by name, a
  short list of payments that settle everything with a "Mark paid" button, and a history.
- **poll** (Polls) — one option per line, single or several answers, who voted for what, close and
  reopen.
- **availability** (Find a time) — a plan gets possible times from the picker, Yes/Maybe/No for each,
  the best one marked, more times can be suggested later.
- **calendar** — an event is a title, a day and time from the picker (or the Today/Tomorrow buttons) and a
  length; upcoming events are grouped under their day, earlier ones below; RSVPs by name.

## What is missing

- App tabs are wired to a real Freya renderer (`app/src/hyper.rs`) and the real
  `hypereffect::client::Client` (CEL included). The command-line harness uses the same client but only
  prints the resolved tree; it has no widgets.
- A reuse/reconnect `hello` is not implemented: a session starts with the wasm `start` event and ends
  when its tab unmounts.
- Guests do not run apps; the guest page syncs the document but has no wasm app host.
- There is no date picker component, so `picker.rs` builds one from selects.
- The apps get no clock or randomness of their own; the host puts `ts`, `seq` and `me` in the action
  event, and new ids use them.
- Anyone in the group can write any key: the document has no per-app permissions.
