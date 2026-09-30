//! Persistent identity: the iroh secret key and the user-editable device name.

use std::path::{Path, PathBuf};

use iroh::SecretKey;

pub struct Settings {
    dir: PathBuf,
    pub secret_key: SecretKey,
    pub name: String,
}

/// Longest name we accept; it has to fit in an mDNS TXT record and a BLE payload.
pub const MAX_NAME_BYTES: usize = 48;

impl Settings {
    pub fn load(dir: &Path) -> Settings {
        let _ = std::fs::create_dir_all(dir);
        let key_path = dir.join("secret.key");
        let secret_key = std::fs::read(&key_path)
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
            .map(|b| SecretKey::from_bytes(&b))
            .unwrap_or_else(|| {
                let key = SecretKey::generate();
                if let Err(err) = std::fs::write(&key_path, key.to_bytes()) {
                    tracing::warn!("could not persist secret key: {err}");
                }
                key
            });
        let name = std::fs::read_to_string(dir.join("name.txt"))
            .ok()
            .map(|s| clean_name(&s))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(default_name);
        Settings { dir: dir.to_owned(), secret_key, name }
    }

    pub fn set_name(&mut self, name: &str) -> String {
        let name = clean_name(name);
        self.name = if name.is_empty() { default_name() } else { name };
        if let Err(err) = std::fs::write(self.dir.join("name.txt"), &self.name) {
            tracing::warn!("could not persist name: {err}");
        }
        self.name.clone()
    }
}

/// Trim, drop control characters and cut to [`MAX_NAME_BYTES`] on a char boundary.
pub fn clean_name(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.trim().chars().filter(|c| !c.is_control()) {
        if out.len() + c.len_utf8() > MAX_NAME_BYTES {
            break;
        }
        out.push(c);
    }
    out.trim().to_owned()
}

/// `<hostname or device model> (<os>)`, e.g. `thinkpad (linux)` or `moto g(7) power (android)`.
pub fn default_name() -> String {
    clean_name(&format!("{} ({})", host(), std::env::consts::OS))
}

#[cfg(not(target_os = "android"))]
fn host() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "device".into())
}

#[cfg(target_os = "android")]
fn host() -> String {
    use std::ffi::{CStr, c_char, c_int};
    unsafe extern "C" {
        fn __system_property_get(name: *const c_char, value: *mut c_char) -> c_int;
    }
    for prop in [c"ro.product.marketname", c"ro.product.model"] {
        let mut buf = [0 as c_char; 92]; // PROP_VALUE_MAX
        let len = unsafe { __system_property_get(prop.as_ptr(), buf.as_mut_ptr()) };
        if len > 0 {
            let s = unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned();
            if !s.is_empty() {
                return s;
            }
        }
    }
    "device".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_cleaned_and_capped() {
        assert_eq!(clean_name("  hi\u{0}there \n"), "hithere");
        assert!(clean_name(&"é".repeat(100)).len() <= MAX_NAME_BYTES);
        assert!(default_name().contains(std::env::consts::OS));
    }
}
