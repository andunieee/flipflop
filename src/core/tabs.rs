//! The group's tabs, which any member shows or hides for everyone (they live in the document): the
//! built-in ones, and apps. An app is a wasm file on offer in the group, hidden from the files tab.
//! Every member fetches it while its tab is on, keeps it in the apps folder (named after its hash,
//! so one copy serves every group) and serves it to the others in turn. An app comes from a file, a
//! link, or another of our groups.

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};

use super::{Core, sharing::open_source};
use crate::{
    apps::{AppParams, Runtime},
    state::Share,
};

/// A wasm module read and kept, waiting for the user to name its tab and answer its form.
#[derive(Debug, Clone, PartialEq)]
pub struct AppDraft {
    /// blake3 of the wasm, its name in the apps folder.
    pub hash: String,
    pub size: u64,
    /// The file's name.
    pub file_name: String,
    pub form: AppParams,
}

/// An app we hold and use in some group, which another group can have without fetching it.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedApp {
    /// blake3 of the wasm, its name in the apps folder.
    pub hash: String,
    /// Its tab's name, as the app named it (or as it was named after its file).
    pub name: String,
    /// The name of the wasm's file.
    pub file_name: String,
    pub size: u64,
    /// The groups it is in.
    pub groups: Vec<String>,
}

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

    /// Read the wasm module at `path`, keep a copy of it, and ask it what it takes to be added to
    /// a group (see [`crate::apps`]). Nothing is shared yet: [`Core::add_app`] does that.
    pub async fn prepare_app(&self, path: PathBuf) -> Result<AppDraft> {
        let file_name = super::sharing::clean_file_name(
            &path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .context("that path has no file name")?,
        );
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
        self.draft_app(file_name, bytes).await
    }

    /// Download the wasm module at `url` (http or https), keep a copy of it, and ask it what it
    /// takes, as [`Core::prepare_app`] does with a file.
    pub async fn prepare_app_url(&self, url: &str) -> Result<AppDraft> {
        let url = reqwest::Url::parse(url.trim()).context("that is not a link")?;
        if !matches!(url.scheme(), "http" | "https") {
            bail!("only http and https links can be downloaded");
        }
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        })
        .with_no_client_auth();
        let client = reqwest::Client::builder()
            .tls_backend_preconfigured(tls)
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(120))
            .build()?;
        let mut response = client
            .get(url.clone())
            .send()
            .await
            .with_context(|| format!("cannot download {url}"))?
            .error_for_status()?;
        if response.content_length().is_some_and(|n| n > MAX_APP_BYTES) {
            bail!("that is too big for an app");
        }
        // Named after the last part of the path, where it was redirected to.
        let file_name = response
            .url()
            .path_segments()
            .and_then(|mut s| s.next_back())
            .filter(|s| !s.is_empty())
            .map_or_else(|| "app.wasm".to_owned(), super::sharing::clean_file_name);
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            bytes.extend_from_slice(&chunk);
            if bytes.len() as u64 > MAX_APP_BYTES {
                bail!("{file_name} is too big for an app");
            }
        }
        self.draft_app(file_name, bytes).await
    }

    /// The apps we hold that are in any of our groups, one per wasm, the latest added first: to add
    /// one of them to another group without fetching it again (see [`Core::prepare_saved_app`]).
    pub fn saved_apps(&self) -> Vec<SavedApp> {
        let mut saved: Vec<(i64, SavedApp)> = Vec::new();
        for group in self.groups().groups {
            let Some((tabs, files)) = self.with_group_doc(&group.id, |d| (d.tabs(), d.files()))
            else {
                continue;
            };
            for app in tabs.into_iter().filter_map(|t| t.app) {
                if !self.app_path(&app.hash).exists() {
                    continue;
                }
                if let Some((_, s)) = saved.iter_mut().find(|(_, s)| s.hash == app.hash) {
                    if !s.groups.contains(&group.id) {
                        s.groups.push(group.id.clone());
                    }
                    continue;
                }
                let file_name = files
                    .iter()
                    .find(|f| f.id == app.file)
                    .map_or_else(|| format!("{}.wasm", app.name), |f| f.name.clone());
                let name = app
                    .name
                    .strip_suffix(".wasm")
                    .unwrap_or(&app.name)
                    .to_owned();
                saved.push((
                    app.ts,
                    SavedApp {
                        hash: app.hash,
                        name,
                        file_name,
                        size: app.size,
                        groups: vec![group.id.clone()],
                    },
                ));
            }
        }
        saved.sort_by_key(|(ts, _)| std::cmp::Reverse(*ts));
        saved.into_iter().map(|(_, s)| s).collect()
    }

    /// Ask a saved app (one of [`Core::saved_apps`]) what it takes, to add it to another group.
    pub async fn prepare_saved_app(&self, app: &SavedApp) -> Result<AppDraft> {
        let bytes = tokio::fs::read(self.app_path(&app.hash))
            .await
            .with_context(|| format!("{} is not saved anymore", app.name))?;
        self.draft_app(app.file_name.clone(), bytes).await
    }

    /// Check that `bytes` are a wasm module, keep them in the apps folder, and ask the app what it
    /// takes.
    async fn draft_app(&self, file_name: String, bytes: Vec<u8>) -> Result<AppDraft> {
        if bytes.len() as u64 > MAX_APP_BYTES {
            bail!("{file_name} is too big for an app");
        }
        if !bytes.starts_with(b"\0asm") {
            bail!("{file_name} is not a WebAssembly module");
        }
        // Hashed here, whatever its file is called: it is offered to the group by this hash.
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let size = bytes.len() as u64;
        let local = self.app_path(&hash);
        if !local.exists() {
            let dir = local.parent().expect("in the apps folder");
            tokio::fs::create_dir_all(dir).await?;
            let part = local.with_extension("part");
            tokio::fs::write(&part, &bytes).await?;
            tokio::fs::rename(&part, &local).await?;
        }
        let form = tokio::task::spawn_blocking(move || Runtime::default().params(&bytes))
            .await?
            .with_context(|| format!("{file_name} does not work as an app"))?;
        Ok(AppDraft {
            hash,
            size,
            file_name,
            form,
        })
    }

    /// Add the app `draft` (from [`Core::prepare_app`]) to the group, with its tab on and the
    /// answers `values` to its form (in the order of `draft.form.params`). Empty answers are the
    /// form's defaults. The tab is named after the file until the app names it itself (see
    /// [`Core::set_app_name`]). Returns the app's id.
    pub async fn add_app(&self, gid: &str, draft: &AppDraft, values: &[String]) -> Result<String> {
        self.require_group(gid)?;
        let params = draft.form.answers(values);
        let stem = draft
            .file_name
            .strip_suffix(".wasm")
            .unwrap_or(&draft.file_name);
        let name = format!("{stem} {}", &draft.hash[..6.min(draft.hash.len())]);
        let local = self.app_path(&draft.hash);
        if !local.exists() {
            bail!("the app's file is gone");
        }
        let fid = self
            .offer_file(gid, local, Some(draft.file_name.clone()), APP_KIND, false)
            .await?;
        let me = self.me();
        let (hash, size) = (&draft.hash, draft.size);
        let id = self.with_doc(gid, |d| {
            d.add_app_tab(&me, &name, &fid, hash, size, &params)
        });
        self.doc_changed(gid, false);
        Ok(id)
    }

    /// The app named its tab: show that name to the whole group.
    pub fn set_app_name(&self, gid: &str, id: &str, name: &str) -> Result<()> {
        self.require_group(gid)?;
        if self.with_doc(gid, |d| d.set_app_name(id, name.trim())) {
            self.doc_changed(gid, false);
        }
        Ok(())
    }

    /// Delete an app from the group, for every member: its tab and its data. If we offered its
    /// wasm, we stop offering it.
    pub fn delete_app(&self, gid: &str, id: &str) -> Result<()> {
        self.require_group(gid)?;
        let Some(app) = self.with_doc(gid, |d| d.app_tab(id)) else {
            bail!("no such app");
        };
        self.with_doc(gid, |d| d.remove_app_tab(id));
        // Only the one who added it can stop the offer; for everyone else it is just hidden.
        let _ = self.stop_file(gid, &app.file);
        self.doc_changed(gid, false);
        Ok(())
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
