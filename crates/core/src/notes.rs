//! An Obsidian-style notes vault: a folder of Markdown files. Links are
//! `[[Name]]`, `[[folder/Name]]`, `[[Name|alias]]` or `[[Name#heading]]`.
//!
//! The walk skips hidden entries (`.obsidian`, `.trash`, `.git`) and every
//! symlink, so nothing outside the vault root is ever listed or read.

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use unchained_protocol::{NoteEntry, SearchHit};

use crate::workspace::Workspace;

pub const MAX_NOTE_BYTES: u64 = 256 * 1024;
const MAX_NOTES: usize = 5000;

/// Every note in the vault, sorted by path.
pub fn list_notes(vault: &Workspace) -> Result<Vec<NoteEntry>> {
    let mut paths = Vec::new();
    walk(vault.root(), "", &mut paths)?;
    paths.sort();
    Ok(paths
        .into_iter()
        .map(|path| NoteEntry {
            title: title_of(&path),
            path,
        })
        .collect())
}

fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<()> {
    let entries = fs::read_dir(dir).with_context(|| dir.display().to_string())?;
    for entry in entries {
        if out.len() >= MAX_NOTES {
            return Ok(());
        }
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if kind.is_dir() {
            walk(&entry.path(), &rel, out)?;
        } else if kind.is_file() && name.ends_with(".md") {
            out.push(rel);
        }
    }
    Ok(())
}

/// The file name without its folder or `.md` extension.
pub fn title_of(path: &str) -> String {
    let file = path.rsplit('/').next().unwrap_or(path);
    file.strip_suffix(".md").unwrap_or(file).to_string()
}

fn check_note_path(rel: &str) -> Result<()> {
    if !rel.ends_with(".md") {
        bail!("notes are Markdown files ending in .md");
    }
    Ok(())
}

pub fn read_note(vault: &Workspace, rel: &str) -> Result<String> {
    check_note_path(rel)?;
    let path = vault.resolve(rel)?;
    let meta = fs::metadata(&path).with_context(|| rel.to_string())?;
    if !meta.is_file() {
        bail!("{rel} is not a note");
    }
    if meta.len() > MAX_NOTE_BYTES {
        bail!(
            "{rel} is {} bytes; the limit is {MAX_NOTE_BYTES}",
            meta.len()
        );
    }
    fs::read_to_string(&path).with_context(|| rel.to_string())
}

pub fn write_note(vault: &Workspace, rel: &str, content: &str) -> Result<usize> {
    check_note_path(rel)?;
    if content.len() as u64 > MAX_NOTE_BYTES {
        bail!(
            "content is {} bytes; the limit is {MAX_NOTE_BYTES}",
            content.len()
        );
    }
    let path = vault.prepare(rel)?;
    if path.is_dir() {
        bail!("{rel} is a folder");
    }
    fs::write(&path, content).with_context(|| rel.to_string())?;
    Ok(content.len())
}

/// Case-insensitive search. Returns at most `limit` matching lines.
pub fn search(vault: &Workspace, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        bail!("the search needs a query");
    }
    let mut hits = Vec::new();
    for note in list_notes(vault)? {
        let Ok(text) = read_note(vault, &note.path) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            if line.to_lowercase().contains(&needle) {
                hits.push(SearchHit {
                    path: note.path.clone(),
                    line: i as u32 + 1,
                    snippet: line.trim().chars().take(200).collect(),
                });
                if hits.len() >= limit {
                    return Ok(hits);
                }
            }
        }
    }
    Ok(hits)
}

/// The targets of every `[[link]]` in `text`, without alias or heading.
pub fn links_in(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("]]") else { break };
        let inner = &after[..end];
        if !inner.contains('\n') {
            let target = inner
                .split('|')
                .next()
                .unwrap_or("")
                .split('#')
                .next()
                .unwrap_or("")
                .trim();
            if !target.is_empty() {
                out.push(target.to_string());
            }
        }
        rest = &after[end + 2..];
    }
    out
}

/// Whether a link points at the note `target` (a vault-relative `.md` path).
/// A bare name matches any folder; a path must match from the vault root.
pub fn link_matches(link: &str, target: &str) -> bool {
    let target = target.strip_suffix(".md").unwrap_or(target);
    let link = link.strip_suffix(".md").unwrap_or(link);
    if link.contains('/') {
        link.eq_ignore_ascii_case(target)
    } else {
        link.eq_ignore_ascii_case(title_of(target).as_str())
    }
}

/// Notes that link to `target`.
pub fn backlinks(vault: &Workspace, target: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for note in list_notes(vault)? {
        if note.path == target {
            continue;
        }
        let Ok(text) = read_note(vault, &note.path) else {
            continue;
        };
        if links_in(&text).iter().any(|l| link_matches(l, target)) {
            out.push(note.path);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("Projects")).unwrap();
        fs::create_dir_all(dir.path().join(".obsidian")).unwrap();
        fs::write(
            dir.path().join("Home.md"),
            "Start at [[Projects/Plan|the plan]] and [[Ideas#later]].\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("Projects/Plan.md"),
            "# Plan\nship the Lantern build\n",
        )
        .unwrap();
        fs::write(dir.path().join("Ideas.md"), "maybe [[plan]]\n").unwrap();
        fs::write(dir.path().join(".obsidian/app.md"), "hidden").unwrap();
        fs::write(dir.path().join("notes.txt"), "not a note").unwrap();
        let ws = Workspace::new("vault", dir.path()).unwrap();
        (dir, ws)
    }

    #[test]
    fn lists_only_visible_markdown() {
        let (_d, ws) = vault();
        let paths: Vec<_> = list_notes(&ws)
            .unwrap()
            .into_iter()
            .map(|n| n.path)
            .collect();
        assert_eq!(paths, vec!["Home.md", "Ideas.md", "Projects/Plan.md"]);
    }

    #[test]
    fn parses_links_and_finds_backlinks() {
        assert_eq!(
            links_in("[[A|x]] [[b#h]] [[c/d]] [[]] [[broken\nline]]"),
            vec!["A", "b", "c/d"]
        );
        let (_d, ws) = vault();
        assert_eq!(
            backlinks(&ws, "Projects/Plan.md").unwrap(),
            vec!["Home.md", "Ideas.md"]
        );
        assert_eq!(backlinks(&ws, "Ideas.md").unwrap(), vec!["Home.md"]);
        assert!(link_matches("Projects/Plan", "Projects/Plan.md"));
        assert!(!link_matches("Other/Plan", "Projects/Plan.md"));
    }

    #[test]
    fn search_reads_lines_case_insensitively() {
        let (_d, ws) = vault();
        let hits = search(&ws, "LANTERN", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(
            (hits[0].path.as_str(), hits[0].line),
            ("Projects/Plan.md", 2)
        );
        assert!(search(&ws, "  ", 10).is_err());
    }

    #[test]
    fn reads_and_writes_stay_in_the_vault() {
        let (d, ws) = vault();
        assert!(read_note(&ws, "../outside.md").is_err());
        assert!(read_note(&ws, "Home.txt").is_err());
        assert!(write_note(&ws, "../escape.md", "x").is_err());
        assert!(write_note(&ws, "Inbox/2026/new.md", "hello").is_ok());
        assert_eq!(read_note(&ws, "Inbox/2026/new.md").unwrap(), "hello");
        assert!(d.path().join("Inbox/2026/new.md").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_listed_or_followed() {
        let (d, ws) = vault();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("leak.md"), "x").unwrap();
        std::os::unix::fs::symlink(outside.path(), d.path().join("link")).unwrap();
        let paths: Vec<_> = list_notes(&ws)
            .unwrap()
            .into_iter()
            .map(|n| n.path)
            .collect();
        assert!(!paths.iter().any(|p| p.contains("leak")));
        assert!(read_note(&ws, "link/leak.md").is_err());
    }
}
