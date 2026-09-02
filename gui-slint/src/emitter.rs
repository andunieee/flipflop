use slint::{ComponentHandle, Weak};
use std::sync::{Arc, Mutex};

use crate::AppWindow;
use crate::State;

/// One node/transfer event: (name, json payload).
pub type MainEvent = (String, Option<String>);

/// Queue of events from the long-lived `NodeService`. Drained by the UI.
#[derive(Clone, Default)]
pub struct MainQueue {
    events: Arc<Mutex<Vec<MainEvent>>>,
}

impl MainQueue {
    pub fn drain(&self) -> Vec<MainEvent> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }

    fn push(&self, name: &str, payload: Option<String>) {
        self.events
            .lock()
            .unwrap()
            .push((name.to_string(), payload));
    }
}

/// `engine::EventEmitter` bound to the node service.
pub struct MainEmitter {
    queue: MainQueue,
}

impl MainEmitter {
    pub fn new(queue: MainQueue) -> Self {
        Self { queue }
    }
}

impl engine::EventEmitter for MainEmitter {
    fn emit_event(&self, event_name: &str) -> Result<(), String> {
        self.queue.push(event_name, None);
        Ok(())
    }

    fn emit_event_with_payload(&self, event_name: &str, payload: &str) -> Result<(), String> {
        self.queue.push(event_name, Some(payload.to_string()));
        Ok(())
    }
}

/// Store of pending per-peer receive cancellers from the auto-accept path.
#[derive(Default, Clone)]
pub struct ReceiveCancels(
    pub Arc<Mutex<std::collections::HashMap<String, tokio::sync::oneshot::Sender<()>>>>,
);

impl ReceiveCancels {
    pub fn lock(
        &self,
    ) -> std::sync::MutexGuard<
        '_,
        std::collections::HashMap<String, tokio::sync::oneshot::Sender<()>>,
    > {
        self.0.lock().unwrap()
    }
}

pub fn handle_event(weak: &Weak<AppWindow>, name: &str, payload: Option<&str>) {
    use crate::format::{fmt_speed, parse_progress};

    let Some(ui) = weak.upgrade() else {
        return;
    };
    let state = ui.global::<State>();

    match name {
        "transfer-started" | "receive-started" => {
            if name == "transfer-started" {
                state.set_send_status("Transferring…".into());
            } else {
                state.set_receive_status("Transferring…".into());
            }
        }
        "share-peer-connected" => {
            state.set_send_peers(state.get_send_peers() + 1);
            state.set_send_status("Receiver connected — transferring…".into());
        }
        "transfer-progress" => {
            if let Some((bytes, total, _speed)) = payload.and_then(parse_progress) {
                if total > 0 {
                    let frac = (bytes as f32 / total as f32).min(1.0);
                    state.set_send_progress(frac);
                    state.set_send_progress_label(format!("{}%", (frac * 100.0) as u32).into());
                }
            }
        }
        "receive-progress" => {
            if let Some((bytes, total, speed)) = payload.and_then(parse_progress) {
                if total > 0 {
                    let frac = (bytes as f32 / total as f32).min(1.0);
                    state.set_receive_progress(frac);
                    state.set_receive_progress_label(format!("{}%", (frac * 100.0) as u32).into());
                }
                state.set_receive_speed(fmt_speed(speed).into());
            }
        }
        "transfer-completed" => {
            state.set_send_progress(1.0);
            state.set_send_progress_label("100%".into());
            state.set_send_status(
                "Receiver got your files — share stays open for more peers.".into(),
            );
        }
        "receive-completed" => {
            state.set_receive_progress(1.0);
            state.set_receive_progress_label("100%".into());
            let out = payload
                .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok())
                .and_then(|v| {
                    v.get("outputDir")
                        .and_then(|v| v.as_str().map(str::to_string))
                })
                .unwrap_or_default();
            state.set_receive_status(format!("Saved to {out}").into());
        }
        "transfer-failed" => {
            state.set_send_status("Transfer to receiver failed.".into());
        }
        "receive-conflicts" => {
            let count = payload
                .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok())
                .and_then(|v| v.as_array().map(|a| a.len()))
                .unwrap_or(0);
            if count > 0 {
                state.set_toast(format!("{count} file(s) renamed to avoid overwriting").into());
                state.set_toast_error(false);
            }
        }
        _ => {}
    }
}
