//! One shared text document per paired peer, an Automerge document that both
//! devices keep in sync for as long as they are online together.
//!
//! The document lives on disk (`<data dir>/shared-docs/<peer id>.automerge`)
//! so it survives restarts and offline edits. Changes travel as Automerge sync
//! messages (`ControlMessage::DocSync`) over the paired control connection:
//! one message is sent when the connection comes up and another after each
//! local edit or received message that has something to say, so only the
//! missing changes cross the wire.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use automerge::sync::{self, SyncDoc};
use automerge::transaction::{CommitOptions, Transactable};
use automerge::{ActorId, AutoCommit, ObjId, ObjType, ReadDoc, ROOT};
use data_encoding::BASE64;
use iroh::endpoint::Connection;

use crate::protocol::{write_message, AppHandle, ControlMessage};

/// Key of the text object in the document root.
const TEXT_KEY: &str = "text";

struct DocState {
    doc: AutoCommit,
    text: ObjId,
    /// Sync progress with the peer on its current connection; reset whenever
    /// that connection is replaced.
    sync: sync::State,
}

struct PeerDoc {
    state: Mutex<DocState>,
    /// Held while a sync message is generated and written, so messages reach
    /// the peer in the order they were generated (the sync protocol assumes
    /// an in-order stream).
    send: tokio::sync::Mutex<()>,
    path: PathBuf,
}

pub struct SharedDocs {
    dir: PathBuf,
    app_handle: AppHandle,
    docs: Mutex<HashMap<String, Arc<PeerDoc>>>,
}

/// The document every peer starts from. Both devices build it independently,
/// so it must be byte-identical (fixed actor, fixed time): otherwise each
/// would create its own text object and one side's first edits would be lost
/// to the other's.
fn initial_doc() -> anyhow::Result<AutoCommit> {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(&[0u8; 16][..]));
    doc.put_object(ROOT, TEXT_KEY, ObjType::Text)?;
    doc.commit_with(CommitOptions::default().with_time(0));
    doc.set_actor(ActorId::random());
    Ok(doc)
}

fn text_obj(doc: &AutoCommit) -> anyhow::Result<ObjId> {
    match doc.get(ROOT, TEXT_KEY)? {
        Some((automerge::Value::Object(ObjType::Text), id)) => Ok(id),
        _ => anyhow::bail!("shared document has no text"),
    }
}

impl PeerDoc {
    fn load(path: PathBuf) -> anyhow::Result<Self> {
        let loaded = match std::fs::read(&path) {
            Ok(bytes) => {
                let loaded = AutoCommit::load(&bytes)
                    .map_err(anyhow::Error::from)
                    .and_then(|doc| Ok((text_obj(&doc)?, doc)));
                match loaded {
                    Ok((text, doc)) => Some((text, doc)),
                    Err(err) => {
                        // Keep what is there rather than overwrite it.
                        tracing::warn!("unreadable shared document {}: {err:#}", path.display());
                        let _ = std::fs::rename(&path, path.with_extension("automerge.bad"));
                        None
                    }
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => return Err(err).context("read shared document"),
        };
        let (text, mut doc) = match loaded {
            Some(loaded) => loaded,
            None => {
                let doc = initial_doc()?;
                (text_obj(&doc)?, doc)
            }
        };
        // A fresh actor per run: two runs never share an op counter.
        doc.set_actor(ActorId::random());
        let this = Self {
            state: Mutex::new(DocState {
                doc,
                text,
                sync: sync::State::new(),
            }),
            send: tokio::sync::Mutex::new(()),
            path,
        };
        // Compact whatever incremental saves piled up.
        this.rewrite()?;
        Ok(this)
    }

    /// Rewrites the whole file, atomically.
    fn rewrite(&self) -> anyhow::Result<()> {
        let bytes = self.state.lock().expect("doc lock").doc.save();
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, bytes).context("write shared document")?;
        std::fs::rename(&tmp, &self.path).context("replace shared document")?;
        Ok(())
    }

    /// Appends what changed since the last save.
    fn append(&self, state: &mut DocState) -> anyhow::Result<()> {
        let bytes = state.doc.save_incremental();
        if bytes.is_empty() {
            return Ok(());
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .context("open shared document")?;
        file.write_all(&bytes).context("append shared document")?;
        Ok(())
    }
}

impl SharedDocs {
    pub fn new(data_dir: &Path, app_handle: AppHandle) -> Self {
        Self {
            dir: data_dir.join("shared-docs"),
            app_handle,
            docs: Mutex::new(HashMap::new()),
        }
    }

    fn open(&self, peer: &str) -> anyhow::Result<Arc<PeerDoc>> {
        let key = peer.to_lowercase();
        anyhow::ensure!(
            key.len() == 64 && key.chars().all(|c| c.is_ascii_hexdigit()),
            "invalid peer id"
        );
        let mut docs = self.docs.lock().expect("docs lock");
        if let Some(doc) = docs.get(&key) {
            return Ok(doc.clone());
        }
        std::fs::create_dir_all(&self.dir).context("create shared document folder")?;
        let doc = Arc::new(PeerDoc::load(self.dir.join(format!("{key}.automerge")))?);
        docs.insert(key, doc.clone());
        Ok(doc)
    }

    /// The text shared with `peer`.
    pub fn text(&self, peer: &str) -> anyhow::Result<String> {
        let doc = self.open(peer)?;
        let state = doc.state.lock().expect("doc lock");
        Ok(state.doc.text(&state.text)?)
    }

    /// Makes the shared text `new_text` (an edit made here). `Ok(false)` when
    /// it already was. The caller pushes the change with [`Self::push`].
    pub fn set_text(&self, peer: &str, new_text: &str) -> anyhow::Result<bool> {
        let doc = self.open(peer)?;
        let mut state = doc.state.lock().expect("doc lock");
        let obj = state.text.clone();
        if state.doc.text(&obj)? == new_text {
            return Ok(false);
        }
        state.doc.update_text(&obj, new_text)?;
        state.doc.commit();
        doc.append(&mut state)?;
        Ok(true)
    }

    /// The connection to `peer` changed: sync from scratch, starting with
    /// our side.
    pub async fn connected(&self, peer: &str, conn: &Connection) {
        let Ok(doc) = self.open(peer) else {
            return;
        };
        doc.state.lock().expect("doc lock").sync = sync::State::new();
        self.push(peer, conn).await;
    }

    /// Sends `peer` whatever it is missing, if anything.
    pub async fn push(&self, peer: &str, conn: &Connection) {
        let Ok(doc) = self.open(peer) else {
            return;
        };
        let _ordered = doc.send.lock().await;
        let message = {
            let mut guard = doc.state.lock().expect("doc lock");
            let state = &mut *guard;
            let message = state.doc.sync().generate_sync_message(&mut state.sync);
            message
        };
        let Some(message) = message else {
            return;
        };
        let data = BASE64.encode(&message.encode());
        let sent = async {
            let (mut send, _recv) = conn.open_bi().await?;
            write_message(&mut send, &ControlMessage::DocSync { data }).await
        }
        .await;
        if let Err(err) = sent {
            tracing::debug!("shared document sync to {peer} failed: {err:#}");
            // That message is lost: sync from scratch when it comes back.
            doc.state.lock().expect("doc lock").sync = sync::State::new();
        }
    }

    /// Applies a sync message from `peer`, tells the UI if the text changed,
    /// and answers on `conn` if there is something to say.
    pub async fn receive(&self, peer: &str, data: &str, conn: &Connection) {
        match self.apply(peer, data) {
            Ok(true) => {
                if let Some(handle) = &self.app_handle {
                    let payload = serde_json::json!({ "endpoint_id": peer.to_lowercase() });
                    let _ = handle.emit_event_with_payload("shared-doc-changed", &payload.to_string());
                }
            }
            Ok(false) => {}
            Err(err) => {
                tracing::warn!("bad shared document sync from {peer}: {err:#}");
                return;
            }
        }
        self.push(peer, conn).await;
    }

    /// Applies one sync message; true when the shared text changed.
    fn apply(&self, peer: &str, data: &str) -> anyhow::Result<bool> {
        let bytes = BASE64
            .decode(data.as_bytes())
            .context("shared document sync is not base64")?;
        let message = sync::Message::decode(&bytes).context("decode shared document sync")?;
        let doc = self.open(peer)?;
        let mut guard = doc.state.lock().expect("doc lock");
        let state = &mut *guard;
        let before = state.doc.get_heads();
        state
            .doc
            .sync()
            .receive_sync_message(&mut state.sync, message)?;
        if state.doc.get_heads() == before {
            return Ok(false);
        }
        // A peer that replaced the text object would leave us nothing to show.
        state.text = text_obj(&state.doc)?;
        doc.append(state)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn docs(dir: &tempfile::TempDir) -> SharedDocs {
        SharedDocs::new(dir.path(), None)
    }

    /// Runs the sync protocol between two stores in memory until quiet.
    fn sync_pair(a: &SharedDocs, b: &SharedDocs) {
        // `a` knows `b` as B and `b` knows `a` as A.
        for store in [(a, B), (b, A)] {
            store.0.open(store.1).unwrap().state.lock().unwrap().sync = sync::State::new();
        }
        let mut turn_a = true;
        let mut idle = 0;
        while idle < 2 {
            let (from, from_peer, to, to_peer) = if turn_a { (a, B, b, A) } else { (b, A, a, B) };
            let message = {
                let doc = from.open(from_peer).unwrap();
                let mut guard = doc.state.lock().unwrap();
                let state = &mut *guard;
                let message = state.doc.sync().generate_sync_message(&mut state.sync);
                message
            };
            match message {
                Some(message) => {
                    idle = 0;
                    to.apply(to_peer, &BASE64.encode(&message.encode())).unwrap();
                }
                None => idle += 1,
            }
            turn_a = !turn_a;
        }
    }

    #[test]
    fn edits_made_apart_merge() {
        let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (a, b) = (docs(&da), docs(&db));

        // Both start from nothing and edit before ever meeting.
        a.set_text(B, "hello").unwrap();
        b.set_text(A, "world").unwrap();
        sync_pair(&a, &b);
        let (ta, tb) = (a.text(B).unwrap(), b.text(A).unwrap());
        assert_eq!(ta, tb);
        assert!(ta.contains("hello") && ta.contains("world"), "{ta}");

        // Later edits flow both ways.
        a.set_text(B, &format!("{ta}!")).unwrap();
        sync_pair(&a, &b);
        assert_eq!(b.text(A).unwrap(), format!("{ta}!"));
    }

    #[test]
    fn edit_is_a_delta_in_the_middle() {
        let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (a, b) = (docs(&da), docs(&db));
        a.set_text(B, "one two three").unwrap();
        sync_pair(&a, &b);
        a.set_text(B, "one 2 three").unwrap();
        b.set_text(A, "one two three four").unwrap();
        sync_pair(&a, &b);
        assert_eq!(a.text(B).unwrap(), "one 2 three four");
        assert_eq!(b.text(A).unwrap(), "one 2 three four");
    }

    #[test]
    fn unchanged_text_is_not_an_edit() {
        let dir = tempfile::tempdir().unwrap();
        let a = docs(&dir);
        assert!(a.set_text(B, "x").unwrap());
        assert!(!a.set_text(B, "x").unwrap());
    }

    #[test]
    fn document_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        {
            let a = docs(&dir);
            a.set_text(B, "kept").unwrap();
            a.set_text(B, "kept, and more").unwrap();
        }
        let a = docs(&dir);
        assert_eq!(a.text(B).unwrap(), "kept, and more");
    }

    #[test]
    fn unicode_text_round_trips() {
        let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (a, b) = (docs(&da), docs(&db));
        a.set_text(B, "héllo 🌍 wörld").unwrap();
        sync_pair(&a, &b);
        b.set_text(A, "héllo 🌎 wörld").unwrap();
        sync_pair(&a, &b);
        assert_eq!(a.text(B).unwrap(), "héllo 🌎 wörld");
    }

    #[test]
    fn rejects_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let a = docs(&dir);
        assert!(a.apply(B, "not base64!").is_err());
        assert!(a.apply(B, &BASE64.encode(b"nope")).is_err());
        assert!(a.text("../etc").is_err());
    }
}
