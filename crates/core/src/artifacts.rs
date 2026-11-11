//! Artifacts: self-contained results the agent makes (a page, a diagram, a
//! document). Each project has its own store, with every saved version kept
//! as its own file, so a re-save never loses the earlier one.
//!
//! Layout: `<artifacts>/<name>/meta.json` and `<artifacts>/<name>/v<N>.<ext>`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Context, Result};
use unchained_protocol::{ArtifactMeta, ArtifactVersion};

use crate::project::now_ms;

pub const MAX_ARTIFACT_BYTES: usize = 256 * 1024;
pub const KINDS: [&str; 4] = ["html", "svg", "markdown", "code"];
const MAX_NAME_CHARS: usize = 64;

/// The MIME type a version is served with, from its kind.
pub fn content_type(kind: &str) -> &'static str {
    match kind {
        "html" => "text/html; charset=utf-8",
        "svg" => "image/svg+xml",
        "markdown" => "text/markdown; charset=utf-8",
        _ => "text/plain; charset=utf-8",
    }
}

fn extension(kind: &str) -> &'static str {
    match kind {
        "html" => "html",
        "svg" => "svg",
        "markdown" => "md",
        _ => "txt",
    }
}

/// Names are lowercase words, so they make safe directory names.
pub fn check_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.chars().count() <= MAX_NAME_CHARS
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !ok {
        bail!("artifact names use a-z, 0-9, '-' and '_' (at most {MAX_NAME_CHARS} characters)");
    }
    Ok(())
}

#[derive(Clone)]
pub struct ArtifactStore {
    root: PathBuf,
    // Serialises writers, so two saves of one name cannot take the same version number.
    lock: Arc<Mutex<()>>,
}

impl ArtifactStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            lock: Arc::default(),
        }
    }

    /// Save a new version of `name`, creating the artifact if it is new.
    pub fn create(&self, name: &str, kind: &str, content: &str) -> Result<ArtifactMeta> {
        check_name(name)?;
        if !KINDS.contains(&kind) {
            bail!("unknown kind '{kind}' (expected html, svg, markdown or code)");
        }
        if content.len() > MAX_ARTIFACT_BYTES {
            bail!(
                "content is {} bytes; the limit is {MAX_ARTIFACT_BYTES}",
                content.len()
            );
        }
        let _guard = self.lock.lock().unwrap();
        let dir = self.root.join(name);
        fs::create_dir_all(&dir).with_context(|| dir.display().to_string())?;
        let mut meta = match read_meta(&dir)? {
            Some(meta) => {
                if meta.kind != kind {
                    bail!(
                        "artifact '{name}' is a {}; save a new name to change its kind",
                        meta.kind
                    );
                }
                meta
            }
            None => ArtifactMeta {
                name: name.to_string(),
                kind: kind.to_string(),
                versions: Vec::new(),
            },
        };
        let version = meta.versions.last().map_or(1, |v| v.version + 1);
        let file = dir.join(version_file(version, kind));
        fs::write(&file, content).with_context(|| file.display().to_string())?;
        meta.versions.push(ArtifactVersion {
            version,
            bytes: content.len() as u64,
            ts_ms: now_ms(),
        });
        write_meta(&dir, &meta)?;
        Ok(meta)
    }

    /// Every artifact in the project, by name.
    pub fn list(&self) -> Result<Vec<ArtifactMeta>> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Ok(out);
        };
        for entry in entries {
            let dir = entry?.path();
            if let Some(meta) = read_meta(&dir)? {
                out.push(meta);
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Read one version (the latest when `version` is `None`).
    pub fn read(&self, name: &str, version: Option<u32>) -> Result<(ArtifactMeta, u32, String)> {
        check_name(name)?;
        let dir = self.root.join(name);
        let meta = read_meta(&dir)?.ok_or_else(|| anyhow!("no artifact named '{name}'"))?;
        let number = match version {
            Some(v) => v,
            None => meta
                .versions
                .last()
                .map(|v| v.version)
                .ok_or_else(|| anyhow!("'{name}' has no versions"))?,
        };
        if !meta.versions.iter().any(|v| v.version == number) {
            bail!("'{name}' has no version {number}");
        }
        let file = dir.join(version_file(number, &meta.kind));
        let text = fs::read_to_string(&file).with_context(|| file.display().to_string())?;
        Ok((meta, number, text))
    }
}

fn version_file(version: u32, kind: &str) -> String {
    format!("v{version}.{}", extension(kind))
}

fn read_meta(dir: &Path) -> Result<Option<ArtifactMeta>> {
    let path = dir.join("meta.json");
    if !path.is_file() {
        return Ok(None);
    }
    let meta =
        serde_json::from_slice(&fs::read(&path)?).with_context(|| path.display().to_string())?;
    Ok(Some(meta))
}

fn write_meta(dir: &Path, meta: &ArtifactMeta) -> Result<()> {
    let path = dir.join("meta.json");
    fs::write(&path, serde_json::to_vec_pretty(meta)?).with_context(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_increment_and_old_ones_remain() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(dir.path().join("artifacts"));
        let first = store.create("chart", "html", "<p>one</p>").unwrap();
        assert_eq!(first.versions.len(), 1);
        let second = store.create("chart", "html", "<p>two</p>").unwrap();
        assert_eq!(
            second
                .versions
                .iter()
                .map(|v| v.version)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );

        assert_eq!(store.read("chart", None).unwrap().2, "<p>two</p>");
        assert_eq!(store.read("chart", Some(1)).unwrap().2, "<p>one</p>");
        assert!(store.read("chart", Some(3)).is_err());
        assert!(store.read("missing", None).is_err());
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn rejects_bad_names_kinds_and_kind_changes() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(dir.path());
        for name in ["", "../x", "Has Space", "a/b", ".hidden"] {
            assert!(store.create(name, "code", "x").is_err(), "{name}");
        }
        assert!(store.create("ok", "exe", "x").is_err());
        assert!(store
            .create("big", "code", &"x".repeat(MAX_ARTIFACT_BYTES + 1))
            .is_err());
        store.create("doc", "markdown", "# hi").unwrap();
        assert!(store.create("doc", "html", "<p>x</p>").is_err());
        assert!(store.create("doc", "markdown", "# hi again").is_ok());
    }
}
