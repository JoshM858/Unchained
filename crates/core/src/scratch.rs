//! Scratch space: each session gets a directory under `<data_dir>/scratch/`
//! for temporary files, outside every project. Deleting the session deletes
//! it. A scratch directory left untouched for a week is archived instead:
//! `<data_dir>/archive/<session>-<ms>.tar.gz`, with a `.json` manifest beside
//! it. Nothing in an archive is read again unless someone restores it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, bail, Context, Result};
use unchained_protocol::ArchiveInfo;

pub const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

pub fn scratch_root(data_dir: &Path) -> PathBuf {
    data_dir.join("scratch")
}

pub fn archive_root(data_dir: &Path) -> PathBuf {
    data_dir.join("archive")
}

fn newest_mtime(path: &Path) -> SystemTime {
    let own = fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let Ok(entries) = fs::read_dir(path) else {
        return own;
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| {
            let p = e.path();
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                newest_mtime(&p)
            } else {
                fs::metadata(&p)
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH)
            }
        })
        .fold(own, |a, b| a.max(b))
}

fn count(path: &Path) -> (u64, u64) {
    let mut files = 0;
    let mut bytes = 0;
    if let Ok(entries) = fs::read_dir(path) {
        for e in entries.filter_map(|e| e.ok()) {
            let p = e.path();
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let (f, b) = count(&p);
                files += f;
                bytes += b;
            } else {
                files += 1;
                bytes += fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    (files, bytes)
}

/// Archive every scratch directory idle for longer than `max_age`, except
/// those of sessions `skip` names (sessions that are running).
pub fn sweep(
    data_dir: &Path,
    now: SystemTime,
    max_age: Duration,
    skip: impl Fn(&str) -> bool,
) -> Result<Vec<ArchiveInfo>> {
    let root = scratch_root(data_dir);
    let Ok(entries) = fs::read_dir(&root) else {
        return Ok(Vec::new());
    };
    let mut done = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if !path.is_dir() {
            continue;
        }
        let Some(session) = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        if skip(&session) {
            continue;
        }
        let idle = now.duration_since(newest_mtime(&path)).unwrap_or_default();
        if idle >= max_age {
            done.push(archive(data_dir, &session, now)?);
        }
    }
    Ok(done)
}

/// Archive one session's scratch directory and remove it.
pub fn archive(data_dir: &Path, session: &str, now: SystemTime) -> Result<ArchiveInfo> {
    let root = scratch_root(data_dir);
    let src = root.join(session);
    if !src.is_dir() {
        bail!("no scratch directory for session {session}");
    }
    let archives = archive_root(data_dir);
    fs::create_dir_all(&archives)?;
    let archived_ms = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let name = format!("{session}-{archived_ms}.tar.gz");
    let (files, bytes) = count(&src);
    let status = Command::new("tar")
        .arg("-czf")
        .arg(archives.join(&name))
        .arg("-C")
        .arg(&root)
        .arg(session)
        .status()
        .context("running tar")?;
    if !status.success() {
        bail!("tar failed while archiving session {session}");
    }
    let info = ArchiveInfo {
        name: name.clone(),
        session: session.to_string(),
        archived_ms,
        files,
        bytes,
    };
    fs::write(
        archives.join(format!("{name}.json")),
        serde_json::to_vec_pretty(&info)?,
    )?;
    fs::remove_dir_all(&src).with_context(|| src.display().to_string())?;
    Ok(info)
}

/// Archives, newest first.
pub fn list_archives(data_dir: &Path) -> Result<Vec<ArchiveInfo>> {
    let Ok(entries) = fs::read_dir(archive_root(data_dir)) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(info) = serde_json::from_slice::<ArchiveInfo>(&fs::read(&path)?) {
            out.push(info);
        }
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.archived_ms));
    Ok(out)
}

/// Put an archive's files back into its session's scratch directory.
/// Returns the session id. Refuses if that directory already exists.
pub fn restore(data_dir: &Path, name: &str) -> Result<String> {
    if name.contains('/')
        || name.contains('\\')
        || name.starts_with('.')
        || !name.ends_with(".tar.gz")
    {
        bail!("not an archive name: {name}");
    }
    let archive_path = archive_root(data_dir).join(name);
    let manifest = archive_root(data_dir).join(format!("{name}.json"));
    if !archive_path.is_file() || !manifest.is_file() {
        bail!("no archive named {name}");
    }
    let info: ArchiveInfo = serde_json::from_slice(&fs::read(&manifest)?)?;
    let root = scratch_root(data_dir);
    let target = root.join(&info.session);
    if target.exists() {
        bail!("session {} already has a scratch directory", info.session);
    }
    fs::create_dir_all(&root)?;
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(&archive_path)
        .arg("-C")
        .arg(&root)
        .status()
        .context("running tar")?;
    if !status.success() {
        return Err(anyhow!("tar failed while restoring {name}"));
    }
    Ok(info.session)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_and_restore_round_trip() {
        let data = tempfile::tempdir().unwrap();
        let dir = scratch_root(data.path()).join("sess1");
        fs::create_dir_all(dir.join("drafts")).unwrap();
        fs::write(dir.join("drafts/a.txt"), "hello scratch").unwrap();

        let now = SystemTime::now();
        // Fresh: nothing is archived.
        assert!(sweep(data.path(), now, MAX_AGE, |_| false)
            .unwrap()
            .is_empty());
        // A week later: archived, and the live directory is gone.
        let later = now + MAX_AGE + Duration::from_secs(1);
        let done = sweep(data.path(), later, MAX_AGE, |_| false).unwrap();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].files, 1);
        assert!(!dir.exists());

        let listed = list_archives(data.path()).unwrap();
        assert_eq!(listed, done);
        let session = restore(data.path(), &listed[0].name).unwrap();
        assert_eq!(session, "sess1");
        assert_eq!(
            fs::read_to_string(dir.join("drafts/a.txt")).unwrap(),
            "hello scratch"
        );
        assert!(restore(data.path(), &listed[0].name).is_err());
        assert!(restore(data.path(), "../etc.tar.gz").is_err());
    }

    #[test]
    fn running_sessions_are_not_archived() {
        let data = tempfile::tempdir().unwrap();
        let dir = scratch_root(data.path()).join("busy");
        fs::create_dir_all(&dir).unwrap();
        let later = SystemTime::now() + MAX_AGE * 2;
        assert!(sweep(data.path(), later, MAX_AGE, |s| s == "busy")
            .unwrap()
            .is_empty());
        assert!(dir.exists());
    }
}
