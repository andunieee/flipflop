//! BLE on Linux via BlueZ (`bluer`): GATT server + advertisement, and a scanning GATT client.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result};
use bluer::{
    AdapterEvent, Address, DiscoveryFilter, DiscoveryTransport, Uuid,
    adv::Advertisement,
    gatt::local::{
        Application, Characteristic, CharacteristicRead, CharacteristicReadRequest, ReqError,
        Service,
    },
};
use futures::StreamExt;
use iroh::EndpointId;

use super::{PAYLOAD_UUID, SERVICE_UUID};
use crate::core::{BlePayload, Core};

pub fn start(core: Core, status: impl Fn(String) + Send + Sync + 'static) {
    let status = Arc::new(status);
    tokio::spawn(async move {
        match run(core, status.clone()).await {
            Ok(()) => {}
            Err(err) => status(format!("Bluetooth unavailable: {err:#}")),
        }
    });
}

async fn run(core: Core, status: Arc<dyn Fn(String) + Send + Sync>) -> Result<()> {
    let service: Uuid = SERVICE_UUID.parse()?;
    let characteristic: Uuid = PAYLOAD_UUID.parse()?;

    let session = bluer::Session::new()
        .await
        .context("connecting to bluetoothd")?;
    let adapter = session
        .default_adapter()
        .await
        .context("no bluetooth adapter")?;
    adapter.set_powered(true).await?;

    // ---- peripheral: serve our payload -------------------------------------------------
    let served: Arc<Mutex<Vec<u8>>> = Arc::default();
    let read_served = served.clone();
    let app = Application {
        services: vec![Service {
            uuid: service,
            primary: true,
            characteristics: vec![Characteristic {
                uuid: characteristic,
                read: Some(CharacteristicRead {
                    read: true,
                    fun: Box::new(move |req: CharacteristicReadRequest| {
                        let data = read_served.lock().unwrap().clone();
                        Box::pin(async move {
                            data.get(req.offset as usize..)
                                .map(<[u8]>::to_vec)
                                .ok_or(ReqError::InvalidOffset)
                        })
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    let _app_handle = adapter.serve_gatt_application(app).await?;
    let _adv_handle = adapter
        .advertise(Advertisement {
            service_uuids: [service].into_iter().collect(),
            discoverable: Some(true),
            local_name: Some("flipflop".into()),
            ..Default::default()
        })
        .await?;

    let mut payload_rx = core.ble_payload();
    let sync_served = served.clone();
    tokio::spawn(async move {
        loop {
            let payload = payload_rx.borrow_and_update().clone();
            if let Some(p) = payload {
                *sync_served.lock().unwrap() = serde_json::to_vec(&p).unwrap_or_default();
            }
            if payload_rx.changed().await.is_err() {
                break;
            }
        }
    });

    // ---- central: scan and read peers' payloads ----------------------------------------
    adapter
        .set_discovery_filter(DiscoveryFilter {
            uuids: [service].into_iter().collect(),
            transport: DiscoveryTransport::Le,
            duplicate_data: false,
            ..Default::default()
        })
        .await?;
    let mut events = adapter.discover_devices().await?;
    status(format!(
        "Bluetooth: advertising and scanning on {}",
        adapter.name()
    ));

    let known: Arc<Mutex<HashMap<Address, EndpointId>>> = Arc::default();
    while let Some(event) = events.next().await {
        match event {
            AdapterEvent::DeviceAdded(addr) => {
                let (core, adapter, known) = (core.clone(), adapter.clone(), known.clone());
                tokio::spawn(async move {
                    // Read once: reconnecting to a device iroh-ble-transport may be using tears its link down.
                    // Name changes reach connected peers through the iroh handshake instead.
                    for attempt in 0..4 {
                        match read_peer(&adapter, addr, service, characteristic).await {
                            Ok(Some(payload)) => {
                                if let Ok(t) =
                                    payload.t.parse::<iroh_tickets::endpoint::EndpointTicket>()
                                {
                                    known.lock().unwrap().insert(addr, t.endpoint_addr().id);
                                }
                                core.ble_seen(payload);
                                break;
                            }
                            Ok(None) => break,
                            Err(err) => {
                                tracing::debug!("ble read {addr} (attempt {attempt}): {err:#}")
                            }
                        }
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                });
            }
            AdapterEvent::DeviceRemoved(addr) => {
                if let Some(id) = known.lock().unwrap().remove(&addr) {
                    core.ble_gone(id);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

async fn read_peer(
    adapter: &bluer::Adapter,
    addr: Address,
    service: Uuid,
    characteristic: Uuid,
) -> Result<Option<BlePayload>> {
    let device = adapter.device(addr)?;
    // The device list may include devices matching only by a stale cache entry.
    if !device.uuids().await?.unwrap_or_default().contains(&service) {
        return Ok(None);
    }
    // Only tear down a link we opened ourselves; it may belong to the iroh BLE transport.
    let we_connected = !device.is_connected().await?;
    if we_connected {
        tokio::time::timeout(Duration::from_secs(15), device.connect())
            .await
            .context("connect timed out")??;
    }
    let result = async {
        for svc in device.services().await? {
            if svc.uuid().await? != service {
                continue;
            }
            for ch in svc.characteristics().await? {
                if ch.uuid().await? == characteristic {
                    let bytes = ch.read().await?;
                    return Ok(serde_json::from_slice(&bytes).ok());
                }
            }
        }
        Ok(None)
    }
    .await;
    if we_connected {
        let _ = device.disconnect().await;
    }
    result
}
