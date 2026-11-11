//! Projects: a named workspace plus the things that belong to it. A project
//! owns its sessions, its artifacts, its instructions file and, optionally,
//! one notes vault. Projects are stored in `<data_dir>/projects.json`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use unchained_protocol::{CreateProject, Project, UpdateProject, VaultMode};

use crate::workspace::Workspace;

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Files read, in order, as project instructions. The first one present wins.
pub const INSTRUCTION_FILES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];
/// Longer instruction files are cut to this many bytes.
pub const MAX_INSTRUCTION_BYTES: usize = 64 * 1024;
const MAX_NAME_CHARS: usize = 80;

/// Project instructions as the agent sees them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instructions {
    pub file: String,
    pub text: String,
}

/// Read the project's instructions from its workspace root, if it has any.
/// Read on every turn, so edits take effect without restarting anything.
pub fn load_instructions(ws: &Workspace) -> Option<Instructions> {
    for name in INSTRUCTION_FILES {
        let Ok(path) = ws.resolve(name) else { continue };
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let bytes = fs::read(&path).ok()?;
        let mut text = String::from_utf8_lossy(&bytes).into_owned();
        if text.len() > MAX_INSTRUCTION_BYTES {
            let mut cut = MAX_INSTRUCTION_BYTES;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
            text.push_str("\n[truncated]");
        }
        return Some(Instructions {
            file: name.to_string(),
            text,
        });
    }
    None
}

/// Shared handle to one project's settings. Sessions hold a clone, so a
/// change to the vault takes effect in existing sessions on their next tool call.
pub type ProjectHandle = Arc<RwLock<Project>>;

/// Owns the project list and persists it.
pub struct ProjectStore {
    path: PathBuf,
    projects: RwLock<BTreeMap<String, ProjectHandle>>,
}

impl ProjectStore {
    pub fn open(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join("projects.json");
        let mut map = BTreeMap::new();
        if path.is_file() {
            let list: Vec<Project> = serde_json::from_slice(
                &fs::read(&path).with_context(|| path.display().to_string())?,
            )
            .with_context(|| format!("parsing {}", path.display()))?;
            for p in list {
                map.insert(p.id.clone(), Arc::new(RwLock::new(p)));
            }
        }
        Ok(Self {
            path,
            projects: RwLock::new(map),
        })
    }

    /// Projects, oldest first.
    pub fn list(&self) -> Vec<Project> {
        let mut list: Vec<Project> = self
            .projects
            .read()
            .unwrap()
            .values()
            .map(|p| p.read().unwrap().clone())
            .collect();
        list.sort_by_key(|p| p.created_ms);
        list
    }

    pub fn get(&self, id: &str) -> Option<ProjectHandle> {
        self.projects.read().unwrap().get(id).cloned()
    }

    pub fn create(&self, input: CreateProject, workspaces: &[Workspace]) -> Result<Project> {
        let name = check_name(&input.name)?;
        if !workspaces.iter().any(|w| w.name() == input.workspace) {
            bail!("unknown workspace '{}'", input.workspace);
        }
        let vault = match input.vault.as_deref() {
            Some(path) => Some(check_vault(path)?),
            None => None,
        };
        let project = Project {
            id: uuid::Uuid::new_v4().simple().to_string(),
            name,
            workspace: input.workspace,
            vault,
            vault_mode: input.vault_mode.unwrap_or_default(),
            created_ms: now_ms(),
        };
        self.projects
            .write()
            .unwrap()
            .insert(project.id.clone(), Arc::new(RwLock::new(project.clone())));
        self.persist()?;
        Ok(project)
    }

    pub fn update(&self, id: &str, input: UpdateProject) -> Result<Project> {
        let handle = self.get(id).ok_or_else(|| anyhow!("no project '{id}'"))?;
        let vault = match input.vault {
            Some(Some(path)) => Some(Some(check_vault(&path)?)),
            Some(None) => Some(None),
            None => None,
        };
        {
            let mut p = handle.write().unwrap();
            if let Some(vault) = vault {
                p.vault = vault;
            }
            if let Some(mode) = input.vault_mode {
                p.vault_mode = mode;
            }
        }
        self.persist()?;
        let updated = handle.read().unwrap().clone();
        Ok(updated)
    }

    /// The project for a workspace, creating one named after it if needed.
    /// Used to adopt sessions written before projects existed.
    pub fn ensure_for_workspace(&self, workspace: &str) -> Result<String> {
        if let Some(p) = self.list().into_iter().find(|p| p.workspace == workspace) {
            return Ok(p.id);
        }
        let project = Project {
            id: uuid::Uuid::new_v4().simple().to_string(),
            name: workspace.to_string(),
            workspace: workspace.to_string(),
            vault: None,
            vault_mode: VaultMode::ReadOnly,
            created_ms: now_ms(),
        };
        let id = project.id.clone();
        self.projects
            .write()
            .unwrap()
            .insert(id.clone(), Arc::new(RwLock::new(project)));
        self.persist()?;
        Ok(id)
    }

    fn persist(&self) -> Result<()> {
        let list = self.list();
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(&list)?)
            .with_context(|| tmp.display().to_string())?;
        fs::rename(&tmp, &self.path).with_context(|| self.path.display().to_string())?;
        Ok(())
    }
}

fn check_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        bail!("a project needs a name");
    }
    if name.chars().count() > MAX_NAME_CHARS {
        bail!("project names are at most {MAX_NAME_CHARS} characters");
    }
    Ok(name.to_string())
}

/// A vault is an existing folder given by absolute path. It is stored
/// canonicalised, so later confinement checks compare real paths.
fn check_vault(path: &str) -> Result<String> {
    let p = Path::new(path);
    if !p.is_absolute() {
        bail!("the vault path must be absolute");
    }
    let canon = p.canonicalize().with_context(|| format!("vault {path}"))?;
    if !canon.is_dir() {
        bail!("vault {path} is not a folder");
    }
    Ok(canon.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instructions_prefer_agents_md_and_truncate() {
        let outer = tempfile::tempdir().unwrap();
        let ws = Workspace::new("w", outer.path()).unwrap();
        assert!(load_instructions(&ws).is_none());
        std::fs::write(outer.path().join("CLAUDE.md"), "claude rules").unwrap();
        assert_eq!(load_instructions(&ws).unwrap().file, "CLAUDE.md");
        std::fs::write(outer.path().join("AGENTS.md"), "agent rules").unwrap();
        let got = load_instructions(&ws).unwrap();
        assert_eq!(
            (got.file.as_str(), got.text.as_str()),
            ("AGENTS.md", "agent rules")
        );
        std::fs::write(
            outer.path().join("AGENTS.md"),
            "é".repeat(MAX_INSTRUCTION_BYTES),
        )
        .unwrap();
        let big = load_instructions(&ws).unwrap();
        assert!(big.text.ends_with("[truncated]"));
    }

    #[test]
    fn projects_persist_and_validate() {
        let data = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let vault = tempfile::tempdir().unwrap();
        let workspaces = vec![Workspace::new("code", root.path()).unwrap()];
        let store = ProjectStore::open(data.path()).unwrap();

        let bad = |input: CreateProject| store.create(input, &workspaces).is_err();
        let base = || CreateProject {
            name: "p".into(),
            workspace: "code".into(),
            vault: None,
            vault_mode: None,
        };
        assert!(bad(CreateProject {
            workspace: "nope".into(),
            ..base()
        }));
        assert!(bad(CreateProject {
            name: "  ".into(),
            ..base()
        }));
        assert!(bad(CreateProject {
            vault: Some("relative".into()),
            ..base()
        }));
        assert!(bad(CreateProject {
            vault: Some(root.path().join("missing").display().to_string()),
            ..base()
        }));

        let p = store
            .create(
                CreateProject {
                    vault: Some(vault.path().display().to_string()),
                    ..base()
                },
                &workspaces,
            )
            .unwrap();
        assert_eq!(p.vault_mode, VaultMode::ReadOnly);

        let store = ProjectStore::open(data.path()).unwrap();
        assert_eq!(store.list(), vec![p.clone()]);

        let updated = store
            .update(
                &p.id,
                UpdateProject {
                    vault: None,
                    vault_mode: Some(VaultMode::ReadWriteAsk),
                },
            )
            .unwrap();
        assert_eq!(updated.vault, p.vault);
        assert_eq!(updated.vault_mode, VaultMode::ReadWriteAsk);
        let detached = store
            .update(
                &p.id,
                UpdateProject {
                    vault: Some(None),
                    vault_mode: None,
                },
            )
            .unwrap();
        assert_eq!(detached.vault, None);
        assert_eq!(detached.vault_mode, VaultMode::ReadWriteAsk);
    }
}
