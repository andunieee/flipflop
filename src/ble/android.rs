//! BLE on Android. The platform BLE APIs need Java subclasses (ScanCallback, BluetoothGattServerCallback),
//! so the work happens in `FlipflopActivity.java`; this module just wires it to the [`Core`].

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

use iroh::EndpointId;

use crate::{
    core::{BlePayload, Core},
    platform,
};

struct Shared {
    core: Core,
    status: Arc<dyn Fn(String) + Send + Sync>,
    addrs: Mutex<HashMap<String, EndpointId>>,
}

static SHARED: OnceLock<Shared> = OnceLock::new();

pub fn start(core: Core, status: impl Fn(String) + Send + Sync + 'static) {
    let mut payload_rx = core.ble_payload();
    let _ = SHARED.set(Shared {
        core,
        status: Arc::new(status),
        addrs: Mutex::default(),
    });
    platform::call_void("bleStart");
    tokio::spawn(async move {
        loop {
            let payload = payload_rx.borrow_and_update().clone();
            if let Some(p) = payload {
                if let Ok(json) = serde_json::to_string(&p) {
                    platform::call_string("bleSetPayload", &json);
                }
            }
            if payload_rx.changed().await.is_err() {
                break;
            }
        }
    });
}

pub fn on_peer(address: String, json: String) {
    let Some(shared) = SHARED.get() else { return };
    let Ok(payload) = serde_json::from_str::<BlePayload>(&json) else {
        return;
    };
    if let Ok(t) = payload.t.parse::<iroh_tickets::endpoint::EndpointTicket>() {
        shared
            .addrs
            .lock()
            .unwrap()
            .insert(address, t.endpoint_addr().id);
    }
    shared.core.ble_seen(payload);
}

pub fn on_lost(address: String) {
    let Some(shared) = SHARED.get() else { return };
    if let Some(id) = shared.addrs.lock().unwrap().remove(&address) {
        shared.core.ble_gone(id);
    }
}

pub fn on_status(text: String) {
    if let Some(shared) = SHARED.get() {
        (shared.status)(text);
    }
}
