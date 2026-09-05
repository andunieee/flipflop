//! Transfer-history recording driven by the same engine events the UI sees.
//! A slim re-implementation of the Tauri shell's `HistoryRecordingEmitter`.

use engine::{
    unix_now_ms, TransferDirection, TransferHistoryStore, TransferPathType, TransferPeer,
    TransferRecord, TransferStatus,
};
use std::sync::{Arc, Mutex};

#[derive(Debug, Default, Clone)]
pub struct Ctx {
    pub root_name: String,
    pub payload_bytes: u64,
    pub item_count: u32,
    pub path_type: Option<TransferPathType>,
    pub save_path: Option<String>,
    pub peer: Option<TransferPeer>,
}

#[derive(Default)]
struct Row {
    id: Option<String>,
    finalized: bool,
    bytes_transferred: u64,
    file_names: Vec<String>,
    peer_count: u32,
}

pub struct Recorder {
    store: Arc<TransferHistoryStore>,
    direction: TransferDirection,
    /// `false` when history recording is turned off in settings: no row is
    /// ever opened, so every later `note`/`finalize` is a no-op.
    enabled: bool,
    ctx: Mutex<Ctx>,
    row: Mutex<Row>,
}

impl Recorder {
    pub fn new(
        store: Arc<TransferHistoryStore>,
        direction: TransferDirection,
        ctx: Ctx,
        enabled: bool,
    ) -> Self {
        Self {
            store,
            direction,
            enabled,
            ctx: Mutex::new(ctx),
            row: Mutex::new(Row::default()),
        }
    }

    pub fn note(&self, event: &str, payload: Option<&str>) {
        match event {
            "transfer-started" | "receive-started" => self.open_row(),
            "transfer-progress" | "receive-progress" => {
                if let Some(bytes) = payload
                    .and_then(|p| p.split(':').next())
                    .and_then(|b| b.parse::<u64>().ok())
                {
                    let mut row = self.row.lock().unwrap_or_else(|p| p.into_inner());
                    row.bytes_transferred = bytes;
                }
            }
            "share-peer-connected" => {
                let mut row = self.row.lock().unwrap_or_else(|p| p.into_inner());
                row.peer_count = row.peer_count.saturating_add(1);
            }
            "receive-file-names" => {
                if let Some(names) = payload.and_then(|p| serde_json::from_str(p).ok()) {
                    let mut row = self.row.lock().unwrap_or_else(|p| p.into_inner());
                    row.file_names = names;
                }
            }
            "transfer-completed" | "receive-completed" => {
                let facts = payload.map(facts_from_payload).unwrap_or_default();
                self.finalize(TransferStatus::Completed, facts.0, facts.1, facts.2, None);
            }
            "transfer-failed" => {
                self.finalize(TransferStatus::Failed, None, None, None, None);
            }
            _ => {}
        }
    }

    /// Closes the row from outside the engine (stop sharing / cancel / error).
    pub fn finalize(
        &self,
        status: TransferStatus,
        duration_ms: Option<u64>,
        export_ms: Option<u64>,
        bytes: Option<u64>,
        error: Option<String>,
    ) {
        let (id, tracked_bytes, file_names, peer_count) = {
            let mut row = self.row.lock().unwrap_or_else(|p| p.into_inner());
            let Some(id) = row.id.clone() else {
                return;
            };
            if row.finalized {
                return;
            }
            row.finalized = true;
            (
                id,
                row.bytes_transferred,
                row.file_names.clone(),
                row.peer_count,
            )
        };

        let is_receive = self.direction == TransferDirection::Receive;
        let completed = matches!(status, TransferStatus::Completed);
        let shape = received_shape(&file_names);

        let result = self.store.update(&id, |record| {
            record.status = status;
            record.ended_at = Some(unix_now_ms());
            record.duration_ms = duration_ms;
            record.export_ms = export_ms;
            match bytes {
                Some(bytes) => {
                    record.payload_bytes = bytes;
                    record.bytes_transferred = bytes;
                }
                None => record.bytes_transferred = tracked_bytes,
            }
            record.avg_speed_bps = match duration_ms {
                Some(ms) if ms > 0 => Some(record.payload_bytes as f64 / (ms as f64 / 1000.0)),
                _ => None,
            };
            if !file_names.is_empty() {
                record.set_file_names(file_names);
            }
            if is_receive && shape.1 > 0 {
                record.root_name = shape.0;
                record.item_count = shape.1;
                record.path_type = shape.2;
            }
            record.peer_count = peer_count.max(u32::from(record.peer.is_some()));
            if record.peer_count > 1 {
                record.peer = None;
            }
            if completed {
                record.resumable_store_path = None;
            }
            record.error = error;
        });

        if let Err(e) = result {
            tracing::warn!("failed to finalize history row: {e}");
        }
    }

    fn open_row(&self) {
        if !self.enabled {
            return;
        }
        let mut row = self.row.lock().unwrap_or_else(|p| p.into_inner());
        if row.id.is_some() {
            return;
        }
        let ctx = self.ctx.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let mut record =
            TransferRecord::new(self.direction, ctx.root_name.clone(), ctx.payload_bytes);
        record.item_count = ctx.item_count;
        record.path_type = ctx.path_type;
        record.save_path = ctx.save_path.clone();
        record.peer = ctx.peer.clone();

        match self.store.open(record) {
            Ok(id) => row.id = Some(id),
            Err(e) => tracing::warn!("failed to open history row: {e}"),
        }
    }
}

/// (durationMs, exportMs, bytes) out of a completion payload.
fn facts_from_payload(payload: &str) -> (Option<u64>, Option<u64>, Option<u64>) {
    match serde_json::from_str::<serde_json::Value>(payload) {
        Ok(value) => (
            value.get("durationMs").and_then(|v| v.as_u64()),
            value.get("exportMs").and_then(|v| v.as_u64()),
            value.get("bytes").and_then(|v| v.as_u64()),
        ),
        Err(_) => (None, None, None),
    }
}

/// (root_name, item_count, path_type) a receive learns from its file list.
fn received_shape(file_names: &[String]) -> (String, u32, Option<TransferPathType>) {
    let mut top_level: Vec<&str> = Vec::new();
    for name in file_names {
        let head = name.split('/').next().unwrap_or(name);
        if !top_level.contains(&head) {
            top_level.push(head);
        }
    }
    match top_level.as_slice() {
        [] => (String::new(), 0, None),
        [only] => {
            let is_dir = file_names.iter().any(|n| n.contains('/'));
            (
                (*only).to_string(),
                1,
                Some(if is_dir {
                    TransferPathType::Directory
                } else {
                    TransferPathType::File
                }),
            )
        }
        many => (String::new(), many.len() as u32, None),
    }
}
