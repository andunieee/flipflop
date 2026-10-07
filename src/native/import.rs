//! Native filesystem import: paths → iroh-blobs store.

use anyhow::{ensure, Context};
use iroh_blobs::api::blobs::AddProgressItem;
use iroh_blobs::{
    api::{
        blobs::{AddPathOptions, ImportMode},
        Store, TempTag,
    },
    format::collection::Collection,
    BlobFormat,
};
use n0_future::StreamExt;
use n0_future::BufferedStreamExt;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use walkdir::WalkDir;

/// Import one or more filesystem paths into the blob store.
pub async fn import_paths(
    paths: Vec<PathBuf>,
    db: &Store,
) -> anyhow::Result<(TempTag, u64, Collection)> {
    let mut entries: Vec<(String, TempTag, u64)> = Vec::new();
    let mut name_seen: BTreeMap<String, usize> = BTreeMap::new();

    for path in paths {
        let stem = path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "item".to_string());

        let import = collect_path_files(&path, &stem)?;
        if import.is_empty() {
            tracing::warn!("no valid files found in path {}, skipping", path.display());
        }

        let mut local = n0_future::stream::iter(import)
            .map(|(name, file_path)| {
                let db = db.clone();
                async move {
                    let import = db.add_path_with_opts(AddPathOptions {
                        path: file_path,
                        mode: ImportMode::TryReference,
                        format: BlobFormat::Raw,
                    });
                    let mut stream = import.stream().await;
                    let mut item_size = 0u64;
                    let temp_tag = loop {
                        let item = stream
                            .next()
                            .await
                            .context("import stream ended without a tag")?;
                        match item {
                            AddProgressItem::Size(size) => item_size = size,
                            AddProgressItem::Done(tt) => break tt,
                            AddProgressItem::Error(cause) => {
                                anyhow::bail!("error importing {}:{}", name, cause)
                            }
                            _ => {}
                        }
                    };
                    anyhow::Ok((name, temp_tag, item_size))
                }
            })
            .buffered_unordered(num_cpus::get())
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<anyhow::Result<Vec<_>>>()?;

        for (name, tag, size) in local.drain(..) {
            let final_name = dedup_name(&name, &mut name_seen);
            entries.push((final_name, tag, size));
        }
    }

    entries.sort_by(|a, b| a.0.cmp(&b.0));
    ensure!(
        !entries.is_empty(),
        "no valid files found in provided paths"
    );
    let total_size = entries.iter().map(|(_, _, size)| *size).sum::<u64>();
    let (collection, tags) = entries
        .into_iter()
        .map(|(name, tag, _)| ((name, tag.hash()), tag))
        .unzip::<_, _, Collection, Vec<_>>();

    let temp_tag = collection.clone().store(db).await?;
    drop(tags);
    Ok((temp_tag, total_size, collection))
}

pub fn canonicalized_path_to_string(
    path: impl AsRef<Path>,
    must_be_relative: bool,
) -> anyhow::Result<String> {
    let mut path_str = String::new();
    let parts = path
        .as_ref()
        .components()
        .filter_map(|c| match c {
            Component::Normal(x) => {
                let c = match x.to_str() {
                    Some(c) => c,
                    None => return Some(Err(anyhow::anyhow!("invalid character in path"))),
                };

                if !c.contains('/') && !c.contains('\\') {
                    Some(Ok(c))
                } else {
                    Some(Err(anyhow::anyhow!("invalid path component {:?}", c)))
                }
            }
            Component::RootDir => {
                if must_be_relative {
                    Some(Err(anyhow::anyhow!("invalid path component {:?}", c)))
                } else {
                    path_str.push('/');
                    None
                }
            }
            _ => Some(Err(anyhow::anyhow!("invalid path component {:?}", c))),
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let parts = parts.join("/");
    path_str.push_str(&parts);
    Ok(path_str)
}

pub fn canonicalize_input_paths(paths: Vec<PathBuf>) -> anyhow::Result<Vec<PathBuf>> {
    use std::collections::BTreeSet;
    let mut uniq = BTreeSet::new();
    for (index, p) in paths.iter().enumerate() {
        let c = p
            .canonicalize()
            .with_context(|| format!("failed to canonicalize path {}", index))?;
        ensure!(c.exists(), "path {} does not exist", index);
        uniq.insert(c);
    }
    let out: Vec<PathBuf> = uniq.into_iter().collect();
    anyhow::ensure!(!out.is_empty(), "no valid paths provided");
    Ok(out)
}

fn dedup_name(name: &str, seen: &mut BTreeMap<String, usize>) -> String {
    match seen.get_mut(name) {
        Some(count) => {
            *count += 1;
            format!("{} ({})", name, count)
        }
        None => {
            seen.insert(name.to_string(), 1);
            name.to_string()
        }
    }
}

fn collect_path_files(path: &Path, root_name: &str) -> anyhow::Result<Vec<(String, PathBuf)>> {
    if path.is_file() {
        let rel = canonicalized_path_to_string(PathBuf::from(root_name), true)?;
        return Ok(vec![(rel, path.to_path_buf())]);
    }

    if path.is_dir() {
        let mut out = Vec::new();
        for (index, entry) in WalkDir::new(path).into_iter().enumerate() {
            let entry = match entry {
                Ok(v) => v,
                Err(_e) => {
                    tracing::warn!("skipping inaccessible entry {}", index);
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let file = entry.path().to_path_buf();
            let rel = file
                .strip_prefix(path)
                .with_context(|| format!("strip_prefix failed for file {}", index))?;
            let mut prefixed = PathBuf::from(root_name);
            prefixed.push(rel);
            let safe = canonicalized_path_to_string(prefixed, true)?;
            out.push((safe, file));
        }
        return Ok(out);
    }
    anyhow::bail!("path is neither file nor directory");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[cfg(unix)]
    #[test]
    fn canonicalized_path_rejects_backslash() {
        let path = Path::new("system-systemd\\x2dcryptsetup.slice");
        assert!(canonicalized_path_to_string(path, true).is_err());
    }

    #[test]
    fn canonicalized_path_accepts_normal() {
        let result = canonicalized_path_to_string(Path::new("subdir/file.txt"), true);
        assert_eq!(result.unwrap(), "subdir/file.txt");
    }

    #[test]
    fn canonicalized_path_rejects_parent_traversal() {
        assert!(canonicalized_path_to_string(Path::new("../etc/passwd"), true).is_err());
    }

    #[test]
    fn canonicalized_path_rejects_absolute_when_relative() {
        assert!(canonicalized_path_to_string(Path::new("/etc/passwd"), true).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn import_skips_invalid_files() {
        use tempfile::TempDir;

        let td = TempDir::new().unwrap();
        let dir = td.path().join("testdir");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("good.txt"), "hello").unwrap();
        std::fs::write(dir.join(format!("bad{}file.txt", '\\')), "bad").unwrap();

        let path = dir.canonicalize().unwrap();
        let root = path.parent().unwrap();
        let data_sources: Vec<(String, PathBuf)> = WalkDir::new(path.clone())
            .into_iter()
            .filter_map(|entry| {
                let entry = entry.ok()?;
                if !entry.file_type().is_file() {
                    return None;
                }
                let path = entry.into_path();
                let relative = path.strip_prefix(root).ok()?;
                canonicalized_path_to_string(relative, true)
                    .ok()
                    .map(|name| (name, path))
            })
            .collect();

        assert_eq!(data_sources.len(), 1, "should skip file with backslash");
        assert!(data_sources[0].0.contains("good.txt"));
    }
}
