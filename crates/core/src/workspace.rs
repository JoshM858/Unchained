//! A workspace is a directory the agent is allowed to touch. Every path a
//! tool receives is resolved through [`Workspace::resolve`], which refuses
//! anything that lands outside the root, including via `..` or symlinks.
//!
//! This is an in-process check, not a sandbox. OS-level confinement of
//! spawned commands is a separate layer (see docs/DESIGN.md).

use std::path::{Component, Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("path escapes the workspace: {0}")]
    Escape(String),
    #[error("{0}: {1}")]
    Io(String, #[source] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct Workspace {
    name: String,
    root: PathBuf,
}

impl Workspace {
    pub fn new(name: impl Into<String>, root: impl AsRef<Path>) -> Result<Self, WorkspaceError> {
        let root = root.as_ref();
        let root = root
            .canonicalize()
            .map_err(|e| WorkspaceError::Io(root.display().to_string(), e))?;
        Ok(Self {
            name: name.into(),
            root,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve a workspace-relative path to an absolute one inside the root.
    /// The final component may not exist yet (so new files can be created),
    /// but its parent directory must.
    pub fn resolve(&self, rel: &str) -> Result<PathBuf, WorkspaceError> {
        let rel_path = Path::new(rel);
        if rel_path.is_absolute()
            || rel_path
                .components()
                .any(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
        {
            return Err(WorkspaceError::Escape(rel.to_string()));
        }
        let joined = self.root.join(rel_path);
        let resolved = match joined.canonicalize() {
            Ok(p) => p,
            Err(_) => {
                let parent = joined
                    .parent()
                    .ok_or_else(|| WorkspaceError::Escape(rel.to_string()))?;
                let name = match joined.components().next_back() {
                    Some(Component::Normal(n)) => n.to_owned(),
                    _ => return Err(WorkspaceError::Escape(rel.to_string())),
                };
                parent
                    .canonicalize()
                    .map_err(|e| WorkspaceError::Io(rel.to_string(), e))?
                    .join(name)
            }
        };
        if !resolved.starts_with(&self.root) {
            return Err(WorkspaceError::Escape(rel.to_string()));
        }
        Ok(resolved)
    }

    /// Like [`resolve`](Self::resolve) for a file that is about to be written:
    /// the path must be plain (no `..`, no absolute parts), its missing parent
    /// directories are created, and the result is then confined as usual.
    pub fn prepare(&self, rel: &str) -> Result<PathBuf, WorkspaceError> {
        let plain = Path::new(rel)
            .components()
            .all(|c| matches!(c, Component::Normal(_)));
        if !plain || rel.is_empty() {
            return Err(WorkspaceError::Escape(rel.to_string()));
        }
        if let Some(parent) = Path::new(rel)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
        {
            std::fs::create_dir_all(self.root.join(parent))
                .map_err(|e| WorkspaceError::Io(rel.to_string(), e))?;
        }
        self.resolve(rel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_creates_parents_and_refuses_escapes() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("ws");
        std::fs::create_dir_all(&root).unwrap();
        let ws = Workspace::new("ws", &root).unwrap();
        let p = ws.prepare("Inbox/2026/note.md").unwrap();
        assert!(p.starts_with(ws.root()));
        assert!(root.join("Inbox/2026").is_dir());
        assert!(ws.prepare("../x.md").is_err());
        assert!(ws.prepare("/abs.md").is_err());
        assert!(ws.prepare("a/../../x.md").is_err());
        assert!(ws.prepare("").is_err());
    }

    #[test]
    fn resolves_inside_and_rejects_escapes() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("ws");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/a.txt"), "x").unwrap();
        std::fs::write(outer.path().join("secret"), "s").unwrap();
        let ws = Workspace::new("ws", &root).unwrap();

        assert!(ws.resolve("sub/a.txt").unwrap().ends_with("sub/a.txt"));
        assert!(ws.resolve("sub/new.txt").is_ok());
        assert!(ws.resolve(".").is_ok());
        assert!(matches!(
            ws.resolve("../secret"),
            Err(WorkspaceError::Escape(_))
        ));
        assert!(matches!(
            ws.resolve("sub/../../secret"),
            Err(WorkspaceError::Escape(_))
        ));
        assert!(matches!(
            ws.resolve("/etc/passwd"),
            Err(WorkspaceError::Escape(_))
        ));
        assert!(ws.resolve("sub/..").is_ok());
        assert!(ws.resolve("missing/new.txt").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_out_of_workspace() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("ws");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(outer.path().join("secret"), "s").unwrap();
        std::os::unix::fs::symlink(outer.path(), root.join("link")).unwrap();
        let ws = Workspace::new("ws", &root).unwrap();
        assert!(matches!(
            ws.resolve("link/secret"),
            Err(WorkspaceError::Escape(_))
        ));
        assert!(matches!(
            ws.resolve("link/new"),
            Err(WorkspaceError::Escape(_))
        ));
    }
}
