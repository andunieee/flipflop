# Experimental miniapps

Five wasm apps for the runtime in `src/apps.rs`: checklists, an expense splitter, a poll, an
availability planner and a shared calendar. They are prototypes of the app layer, not wired into the
desktop or Android UI.

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
  {"name": "add_list", "payload": {"name": "Groceries"}},
  {"name": "add_item", "payload": {"list": "Groceries", "text": "Milk"}},
  {"name": "toggle_item", "payload": {"list": "Groceries", "item": "Milk"}}
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

## The apps

Each module's doc comment has its exact state keys and actions.

- **checklist** — lists and items, add/remove, done/undo, clear done. Items that arrive before their
  list are kept and show once the list is named.
- **expenses** — integer cents, payer, participants, settlements; balances and a greedy set of
  transfers, each with a Settle button. Currency is a label only.
- **poll** — options, single- or multi-choice, per-voter ballots keyed by timestamp so the latest
  vote wins; closed polls ignore later votes.
- **availability** — plans with slots, yes/maybe/no per peer, score (2/1) and the best slot.
- **calendar** — events with start/end (epoch ms), place, note, author, RSVP yes/maybe/no, sorted by
  start (a new earlier event is inserted with `before`).

## What is missing

- App tabs are wired to a real Freya renderer (`app/src/hyper.rs`) and the real
  `hypereffect::client::Client` (CEL included). The command-line harness uses the same client but only
  prints the resolved tree; it has no widgets.
- A reuse/reconnect `hello` is not implemented: a session starts with the wasm `start` event and ends
  when its tab unmounts.
- Guests do not run apps; the guest page syncs the document but has no wasm app host.
- The apps get no clock or randomness of their own; the host puts `ts`, `seq` and `me` in the action
  event, and new ids use them.
- Anyone in the group can write any key: the document has no per-app permissions.
