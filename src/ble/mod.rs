//! Bluetooth LE presence: every device advertises a GATT service whose single readable
//! characteristic holds a [`BlePayload`](crate::core::BlePayload) (name + iroh ticket), and scans
//! for the same service. A peer found this way shows up in the list and connects over iroh
//! using the address in its ticket.

/// 128-bit UUID of the flipflop GATT service.
pub const SERVICE_UUID: &str = "f11bf10b-0000-4f1f-9b0a-8e2d6e7a0001";
/// Readable characteristic carrying the JSON payload.
pub const PAYLOAD_UUID: &str = "f11bf10b-0000-4f1f-9b0a-8e2d6e7a0002";

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::start;

#[cfg(target_os = "android")]
pub mod android;
#[cfg(target_os = "android")]
pub use android::start;

#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub fn start(_core: crate::core::Core, status: impl Fn(String) + Send + Sync + 'static) {
    status("Bluetooth: not supported on this platform yet".into());
}
