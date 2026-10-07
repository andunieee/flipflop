//! Native filesystem blob store creation.

use crate::native::types::AutoCleanupDir;
use anyhow::Context;
use iroh_blobs::store::fs::FsStore;
use std::path::PathBuf;

/// Root for every blob store this crate creates. Set once by the shell
/// (Android points it at the app cache dir, where `std::env::temp_dir()` is
/// not writable before Android 13). Left unset, [`temp_dir`] falls back to
/// `std::env::temp_dir()`, so tests and non-Tauri consumers keep working.
pub static TEMP_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Directory name prefix of every partial-receive blob store.
pub const RECV_DIR_PREFIX: &str = ".dashbeam-recv-";
/// Prefixes older builds used. Never created any more; the shell still sweeps
/// them at launch so an upgrade leaves nothing behind in the temp dir. Sends
/// now go through the node's own store (see `shares`).
pub const LEGACY_DIR_PREFIXES: [&str; 3] = [".sendme-send-", ".sendme-recv-", ".dashbeam-send-"];

pub async fn create_recv_store(hash_hex: &str) -> anyhow::Result<(FsStore, PathBuf)> {
    let dir_name = format!("{RECV_DIR_PREFIX}{hash_hex}");
    let path = temp_dir().join(dir_name);
    let store = FsStore::load(&path)
        .await
        .with_context(|| format!("failed to load recv store at {}", path.display()))?;
    Ok((store, path))
}

pub fn recv_cleanup_guard(path: PathBuf) -> AutoCleanupDir {
    AutoCleanupDir::new(path)
}

pub fn temp_dir() -> PathBuf {
    TEMP_DIR.get().cloned().unwrap_or_else(std::env::temp_dir)
}
