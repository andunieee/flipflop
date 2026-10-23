//! Desktop webcam QR scanner (Android uses a system scanner activity instead, see `platform`).

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

use nokhwa::{
    Camera,
    pixel_format::RgbFormat,
    utils::{CameraIndex, RequestedFormat, RequestedFormatType},
};

/// A running scan; stops when dropped or [`Scanner::stop`] is called.
pub struct Scanner(Arc<AtomicBool>);

impl Scanner {
    pub fn stop(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Open the default camera on a background thread. `on_frame` gets `(width, height, rgba)` for the
/// preview; `on_done` is called exactly once with the decoded text or an error message.
pub fn start(
    on_frame: impl Fn(u32, u32, Vec<u8>) + Send + 'static,
    on_done: impl FnOnce(Result<String, String>) + Send + 'static,
) -> Scanner {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    thread::spawn(move || on_done(capture(&flag, on_frame)));
    Scanner(stop)
}

fn capture(
    stop: &AtomicBool,
    on_frame: impl Fn(u32, u32, Vec<u8>),
) -> Result<String, String> {
    let format = RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate);
    let mut camera = Camera::new(CameraIndex::Index(0), format).map_err(|e| format!("camera: {e}"))?;
    camera.open_stream().map_err(|e| format!("camera: {e}"))?;
    let mut n = 0u32;
    while !stop.load(Ordering::Relaxed) {
        let frame = camera.frame().map_err(|e| format!("camera: {e}"))?;
        let img = frame.decode_image::<RgbFormat>().map_err(|e| format!("camera: {e}"))?;
        let (w, h) = (img.width(), img.height());
        let rgb = img.as_raw();
        n += 1;
        if n % 3 == 0 {
            let gray: Vec<u8> = rgb
                .chunks_exact(3)
                .map(|p| ((p[0] as u32 * 77 + p[1] as u32 * 150 + p[2] as u32 * 29) >> 8) as u8)
                .collect();
            if let Some(text) = crate::qr::decode_gray(w as usize, h as usize, &gray) {
                return Ok(text);
            }
        }
        let rgba = rgb.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
        on_frame(w, h, rgba);
    }
    Err("cancelled".into())
}
