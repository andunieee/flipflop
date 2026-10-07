//! The group's tabs, which any member shows or hides for everyone (they live in the document): the
//! built-in ones, and apps. An app is a wasm file on offer in the group, hidden from the files tab.
//! Every member fetches it while its tab is on, keeps it in the apps folder (named after its hash,
//! so one copy serves every group) and serves it to the others in turn.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use super::{Core, sharing::open_source};
use crate::state::Share;

/// The kind of an app's offer in the group's files.
pub(super) const APP_KIND: &str = "wasm";
const MAX_APP_BYTES: u64 = 32 << 20;

impl Core {
    /// Show or hide one of the group's tabs, for every member.
    pub fn set_tab(&self, gid: &str, tab: &str, on: bool) -> Result<()> {
        self.require_group(gid)?;
        if !self.with_doc(gid, |d| d.set_tab(tab, on)) {
            bail!("no such tab");
        }
        self.doc_changed(gid, false);
        Ok(())
    }

    /// Add the wasm module at `path` to the group as an app, with its tab on. `name` defaults to
    /// the file's. Returns the app's id.
    pub async fn add_app(&self, gid: &str, path: PathBuf, name: Option<String>) -> Result<String> {
        self.require_group(gid)?;
        let name = super::sharing::clean_file_name(&match name {
            Some(name) => name,
            None => path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .context("that path has no file name")?,
        });
        let reading = path.clone();
        let bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
            use std::io::Read;
            let mut bytes = Vec::new();
            open_source(&reading)
                .with_context(|| format!("cannot read {}", reading.display()))?
                .take(MAX_APP_BYTES + 1)
                .read_to_end(&mut bytes)?;
            Ok(bytes)
        })
        .await??;
        // We keep a copy; a picked file's read permission is not needed anymore.
        #[cfg(target_os = "android")]
        if path.to_str().is_some_and(|p| p.starts_with("content://")) {
            crate::platform::release_content_uri(&path.to_string_lossy());
        }
        if bytes.len() as u64 > MAX_APP_BYTES {
            bail!("{name} is too big for an app");
        }
        if !bytes.starts_with(b"\0asm") {
            bail!("{name} is not a WebAssembly module");
        }
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let local = self.app_path(&hash);
        if !local.exists() {
            let dir = local.parent().expect("in the apps folder");
            tokio::fs::create_dir_all(dir).await?;
            let part = local.with_extension("part");
            tokio::fs::write(&part, &bytes).await?;
            tokio::fs::rename(&part, &local).await?;
        }
        let fid = self
            .offer_file(gid, local, Some(name.clone()), APP_KIND, false)
            .await?;
        let me = self.me();
        let size = bytes.len() as u64;
        let id = self.with_doc(gid, |d| d.add_app_tab(&me, &name, &fid, &hash, size));
        self.doc_changed(gid, false);
        Ok(id)
    }

    /// Where we keep the wasm with this hash.
    pub(super) fn app_path(&self, hash: &str) -> PathBuf {
        self.0.sharing.apps_dir.join(format!("{hash}.wasm"))
    }

    /// For every app whose tab is on: fetch its wasm if we lack it, serve it if we have it.
    pub(super) fn app_jobs(&self, gid: &str) {
        let me = self.me();
        let (tabs, files) = self.with_doc(gid, |d| (d.tabs(), d.files()));
        for app in tabs.iter().filter(|t| t.on).filter_map(|t| t.app.as_ref()) {
            // Not synced yet, or stopped: nothing to fetch, and nobody may serve it.
            let Some(offer) = files.iter().find(|f| f.id == app.file && !f.stopped) else {
                continue;
            };
            if offer.from == me {
                continue;
            }
            let local = self.app_path(&app.hash);
            if local.exists() {
                if !offer.hosts.contains(&me) {
                    self.serve_app(gid, &offer.id, local);
                }
                continue;
            }
            let online = std::iter::once(&offer.from)
                .chain(&offer.hosts)
                .filter(|id| **id != me)
                .any(|id| id.parse().ok().and_then(|id| self.connection(id)).is_some());
            if online {
                let (this, gid, fid) = (self.clone(), gid.to_owned(), offer.id.clone());
                tokio::spawn(async move {
                    // Failures show on the tab and are retried later.
                    let _ = this.download(&gid, &fid, false).await;
                });
            }
        }
    }

    /// We hold the app's wasm (at `path`): count it as downloaded, and serve it to the group.
    pub(super) fn serve_app(&self, gid: &str, fid: &str, path: PathBuf) {
        let me = self.me();
        self.with_state(|st| {
            for list in [&mut st.received, &mut st.shares] {
                if !list.iter().any(|s| s.group == gid && s.file == fid) {
                    list.push(Share {
                        group: gid.to_owned(),
                        file: fid.to_owned(),
                        path: path.clone(),
                        staged: false,
                    });
                }
            }
        });
        self.with_doc(gid, |d| {
            d.mark_downloaded(fid, &me);
            d.set_hosting(fid, &me, true);
        });
        self.doc_changed(gid, false);
    }
}
