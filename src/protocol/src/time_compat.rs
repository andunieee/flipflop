//! `std::time::Instant` and `tokio::time` panic on `wasm32-unknown-unknown`.
//! Use `web-time` for monotonic clocks in the browser.

#[cfg(not(target_arch = "wasm32"))]
pub use std::time::{Duration, Instant};

#[cfg(target_arch = "wasm32")]
pub use web_time::{Duration, Instant};

#[cfg(not(target_arch = "wasm32"))]
pub async fn sleep(duration: Duration) {
    tokio::time::sleep(duration).await;
}

/// Yield-based sleep that stays `Send` (required by iroh protocol handlers on wasm).
#[cfg(target_arch = "wasm32")]
pub async fn sleep(duration: Duration) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        tokio::task::yield_now().await;
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn timeout<F, T>(duration: Duration, future: F) -> Result<T, tokio::time::error::Elapsed>
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(duration, future).await
}

/// JS timer futures are `!Send`; run the inner future directly on wasm.
#[cfg(target_arch = "wasm32")]
pub async fn timeout<F, T>(duration: Duration, future: F) -> Result<T, TimeoutElapsed>
where
    F: std::future::Future<Output = T>,
{
    let _ = duration;
    Ok(future.await)
}

/// Stand-in for `tokio::time::error::Elapsed` on wasm32.
#[cfg(target_arch = "wasm32")]
#[derive(Debug, Clone, Copy)]
pub struct TimeoutElapsed;

#[cfg(target_arch = "wasm32")]
impl std::fmt::Display for TimeoutElapsed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("timeout elapsed")
    }
}

#[cfg(target_arch = "wasm32")]
impl std::error::Error for TimeoutElapsed {}
