//! `weft changes`: the notes created, modified and deleted in a stretch of
//! git history, as the lists obsidian-list-modified keeps in daily notes.
//!
//! Only reads. Where the lists go is the caller's decision.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::vault::{self, Vault};

/// What happened to one file over the range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Created,
    Modified,
    Deleted,
}

/// Each file's change over the range, from its git statuses (`A`, `M`, `D`,
/// `T`) in order. A file created and deleted inside the range is left out.
pub fn classify<'a>(events: impl IntoIterator<Item = (char, &'a str)>) -> BTreeMap<&'a str, Change> {
    let mut seen: BTreeMap<&str, (char, char)> = BTreeMap::new();
    for (status, path) in events {
        seen.entry(path).and_modify(|e| e.1 = status).or_insert((status, status));
    }
    seen.into_iter()
        .filter_map(|(path, (first, last))| {
            let change = match (first == 'A', last == 'D') {
                (true, true) => return None,
                (true, false) => Change::Created,
                (false, true) => Change::Deleted,
                (false, false) => Change::Modified,
            };
            Some((path, change))
        })
        .collect()
}

/// The three blocks, in the order and form obsidian-list-modified writes
/// them, or `None` when no note changed. Created and modified notes are
/// linked the way the server writes links from `into`, in the vault's
/// syntax; a note no longer in the vault, and every deleted one, by its path.
pub fn render(vault: &Vault, changes: &BTreeMap<&str, Change>, into: &str) -> Option<String> {
    if changes.is_empty() {
        return None;
    }
    let mut out = String::new();
    for (label, kind) in [("CREATED", Change::Created), ("MODIFIED", Change::Modified), ("DELETED", Change::Deleted)] {
        out.push_str(&format!("%% LIST {label} %%\n"));
        for (rel, _) in changes.iter().filter(|(_, c)| **c == kind) {
            let link = match vault.note(rel) {
                Some(_) if kind != Change::Deleted => vault.write_link(rel, into),
                _ => crate::parse::write_link(&vault.syntax, &rel[..rel.len() - 3], rel),
            };
            out.push_str(&format!("- {link}\n"));
        }
        out.push_str("%% END %%\n");
    }
    Some(out)
}

/// Whether a changed path belongs in the lists: a note, outside hidden
/// folders, not excluded, not the note the lists go into.
fn listed(rel: &str, exclude: &Gitignore, into: &str) -> bool {
    vault::is_note(rel)
        && !rel.split('/').any(|p| p.starts_with('.'))
        && !exclude.matched_path_or_any_parents(rel, false).is_ignore()
        && rel != into
}

const USAGE: &str = "usage: weft changes <since> <until> [--into <note>] [--exclude <pattern>]...

Prints the notes created, modified and deleted in commits from <since> up to
<until> (any date git accepts, such as \"2026-11-19 00:00\"), as the
%% LIST CREATED/MODIFIED/DELETED %% blocks obsidian-list-modified writes.
Prints nothing when no note changed. Run it inside the vault.

  --into <note>        the note the lists will go into, from the vault root;
                       links are written from it, and it is not listed
  --exclude <pattern>  leave out notes matching a .gitignore-style pattern";

/// Runs `weft changes` with the arguments after `changes`.
pub fn main(args: &[String]) -> Result<(), String> {
    let mut dates = Vec::new();
    let mut into = String::new();
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let (root, kind) = vault::discover(&cwd.join("note.md"));
    let mut exclude = GitignoreBuilder::new(&root);
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--into" => into = args.next().ok_or(USAGE)?.clone(),
            "--exclude" => {
                exclude.add_line(None, args.next().ok_or(USAGE)?).map_err(|e| e.to_string())?;
            }
            "-h" | "--help" => return Err(USAGE.into()),
            _ if arg.starts_with('-') => return Err(USAGE.into()),
            _ => dates.push(arg.clone()),
        }
    }
    let [since, until] = dates.as_slice() else { return Err(USAGE.into()) };
    let exclude = exclude.build().map_err(|e| e.to_string())?;

    let log = git_log(&root, since, until)?;
    let events = parse_log(&log).into_iter().filter(|(_, p)| listed(p, &exclude, &into));
    let changes = classify(events);
    if changes.is_empty() {
        return Ok(());
    }
    let vault = Vault::load(root, kind, None);
    if let Some(out) = render(&vault, &changes, &into) {
        print!("{out}");
    }
    Ok(())
}

fn git_log(root: &Path, since: &str, until: &str) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "core.quotePath=false", "log", "--reverse", "--no-renames", "--name-status", "-z", "--format="])
        .arg(format!("--since={since}"))
        .arg(format!("--until={until}"))
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    String::from_utf8(out.stdout).map_err(|_| "git printed a path that is not UTF-8".into())
}

/// `git log --name-status -z` output as (status, path), oldest first.
fn parse_log(log: &str) -> Vec<(char, &str)> {
    let mut out = Vec::new();
    let mut tokens = log.split('\0').map(|t| t.trim_start_matches('\n')).filter(|t| !t.is_empty());
    while let Some(status) = tokens.next() {
        let Some(path) = tokens.next() else { break };
        if let Some(c) = status.chars().next() {
            out.push((c, path));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_a_range() {
        let changes = classify([
            ('A', "new.md"),
            ('M', "new.md"),
            ('M', "old.md"),
            ('D', "gone.md"),
            ('A', "brief.md"),
            ('D', "brief.md"),
            ('D', "back.md"),
            ('A', "back.md"),
        ]);
        let want = [
            ("back.md", Change::Modified),
            ("gone.md", Change::Deleted),
            ("new.md", Change::Created),
            ("old.md", Change::Modified),
        ];
        assert_eq!(changes.into_iter().collect::<Vec<_>>(), want);
    }

    #[test]
    fn parses_name_status() {
        let log = "\nA\0a b.md\0M\0x/\"q\".md\0\0\nD\0é.md\0";
        assert_eq!(parse_log(log), [('A', "a b.md"), ('M', "x/\"q\".md"), ('D', "é.md")]);
    }

    #[test]
    fn renders_like_list_modified() {
        let v = Vault::from_files(&[("Array.md", ""), ("School/Array.md", ""), ("Plan.md", "")]);
        let changes = BTreeMap::from([
            ("Plan.md", Change::Created),
            ("School/Array.md", Change::Modified),
            ("Old/Thing.md", Change::Deleted),
            ("Renamed since.md", Change::Modified),
        ]);
        let want = "%% LIST CREATED %%\n- [[Plan]]\n%% END %%\n\
                    %% LIST MODIFIED %%\n- [[Renamed since]]\n- [[School/Array]]\n%% END %%\n\
                    %% LIST DELETED %%\n- [[Old/Thing]]\n%% END %%\n";
        assert_eq!(render(&v, &changes, "2026/11/19.md").as_deref(), Some(want));
        assert_eq!(render(&v, &BTreeMap::new(), ""), None);
        let v = Vault::weft_from_files(&[("Plan.md", ""), ("2026/11/18.md", ""), ("2026/11/19.md", "")]);
        let changes = BTreeMap::from([("Plan.md", Change::Created), ("2026/11/18.md", Change::Modified)]);
        let want = "%% LIST CREATED %%\n- [Plan]\n%% END %%\n%% LIST MODIFIED %%\n- @2026.11.18\n%% END %%\n\
                    %% LIST DELETED %%\n%% END %%\n";
        assert_eq!(render(&v, &changes, "2026/11/19.md").as_deref(), Some(want));
    }

    #[test]
    fn lists_only_notes() {
        let mut b = GitignoreBuilder::new("/vault");
        b.add_line(None, "[0-9][0-9][0-9][0-9]/[0-9][0-9]/[0-9][0-9].md").unwrap();
        let ex = b.build().unwrap();
        assert!(listed("Plan.md", &ex, "2026/11/19.md"));
        assert!(!listed("attachments/a.png", &ex, ""));
        assert!(!listed(".obsidian/x.md", &ex, ""));
        assert!(!listed("2026/11/18.md", &ex, ""));
        assert!(!listed("Plan.md", &ex, "Plan.md"));
    }
}
