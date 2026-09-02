use crate::recorder::Recorder;
use slint::{ComponentHandle, Weak};
use std::sync::Arc;

use crate::AppWindow;
use crate::State;

/// Bridges engine events to the Slint UI and the history recorder.
pub struct GuiEmitter {
    weak: Weak<AppWindow>,
    recorder: Option<Arc<Recorder>>,
}

impl GuiEmitter {
    pub fn new(weak: Weak<AppWindow>, recorder: Option<Arc<Recorder>>) -> Self {
        Self { weak, recorder }
    }

    fn dispatch(&self, name: &str, payload: Option<String>) {
        if let Some(recorder) = &self.recorder {
            recorder.note(name, payload.as_deref());
        }
        let weak = self.weak.clone();
        let name = name.to_string();
        let _ = slint::invoke_from_event_loop(move || {
            handle_event(&weak, &name, payload.as_deref());
        });
    }
}

impl engine::EventEmitter for GuiEmitter {
    fn emit_event(&self, event_name: &str) -> Result<(), String> {
        self.dispatch(event_name, None);
        Ok(())
    }

    fn emit_event_with_payload(&self, event_name: &str, payload: &str) -> Result<(), String> {
        self.dispatch(event_name, Some(payload.to_string()));
        Ok(())
    }
}

fn handle_event(weak: &Weak<AppWindow>, name: &str, payload: Option<&str>) {
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
            state.set_receive_done(true);
            let out = payload
                .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok())
                .and_then(|v| {
                    v.get("outputDir")
                        .and_then(|v| v.as_str().map(str::to_string))
                })
                .unwrap_or_default();
            state.set_received_dir(out.clone().into());
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
