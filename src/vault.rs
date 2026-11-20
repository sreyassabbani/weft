//! Files, names, resolution and link text.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::parse::{self, Kind, Link, Note, Segment};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Root {
    /// A folder with `.obsidian`: every non-hidden file is part of it.
    Obsidian,
    /// A repository: Markdown files not ignored by `.gitignore`.
    Repo,
    /// A file with no vault around it.
    Single,
}

/// Where a link goes.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Dest {
    /// A file in the vault, by its path from the root.
    File(String),
    /// A note not written yet, by its lowercased path without `.md`.
    Unwritten(String),
}

pub struct File {
    /// Markdown files only.
    pub text: Option<String>,
    pub note: Note,
    /// The editor's version, while the file is open.
    pub open: Option<i32>,
    stamp: Option<(SystemTime, u64)>,
}

pub struct Vault {
    pub root: PathBuf,
    pub kind: Root,
    /// Paths from the root, with `/` separators and extensions.
    pub files: BTreeMap<String, File>,
    pub index: Index,
}

/// The names in a vault, for resolving links. Built from paths alone, so it
/// can also describe the vault as it will be after a move.
#[derive(Default)]
pub struct Index {
    lower: HashMap<String, String>,
    /// Link name (stem for notes, file name otherwise), lowercased, to paths.
    names: HashMap<String, Vec<String>>,
}

/// File types Obsidian embeds; a link to one of these is not a note.
const ATTACHMENTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "svg", "webp", "avif", "mp3", "webm", "wav", "m4a", "ogg",
    "3gp", "flac", "mp4", "ogv", "mov", "mkv", "pdf", "canvas", "base",
];

pub fn is_note(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".md")
}

pub fn is_attachment(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| ATTACHMENTS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// The name a link uses: the file name, without `.md` for notes.
pub fn link_name(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    if is_note(name) { &name[..name.len() - 3] } else { name }
}

fn dir(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..i])
}

/// Normalizes a heading for comparison, the way Obsidian matches heading
/// links: case-insensitive, with the characters it strips turned to spaces.
pub fn heading_key(text: &str) -> String {
    let mapped: String = text
        .chars()
        .map(|c| if "#|^:%[]".contains(c) { ' ' } else { c })
        .collect();
    mapped.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// The anchor GitHub generates for a heading, used by Markdown links.
fn slug(text: &str) -> String {
    text.trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

/// Finds the vault containing `path`: the nearest ancestor with `.obsidian`,
/// else the nearest with `.git`, `.jj` or `.hg`.
pub fn discover(path: &Path) -> (PathBuf, Root) {
    let parent = path.parent().unwrap_or(path);
    if let Some(root) = parent.ancestors().find(|d| d.join(".obsidian").is_dir()) {
        return (root.to_path_buf(), Root::Obsidian);
    }
    for d in parent.ancestors() {
        if [".git", ".jj", ".hg"].iter().any(|m| d.join(m).exists()) {
            return (d.to_path_buf(), Root::Repo);
        }
    }
    (parent.to_path_buf(), Root::Single)
}

impl Vault {
    /// Loads the vault at `root`. For `Root::Single`, `only` is the one file.
    pub fn load(root: PathBuf, kind: Root, only: Option<&str>) -> Vault {
        let mut vault = Vault {
            root,
            kind,
            files: BTreeMap::new(),
            index: Index::default(),
        };
        let paths = match (kind, only) {
            (Root::Single, Some(only)) => vec![only.to_string()],
            _ => vault.walk(),
        };
        for rel in paths {
            let file = vault.read(&rel);
            vault.files.insert(rel, file);
        }
        vault.reindex();
        vault
    }

    /// An empty vault, for tests.
    pub fn from_files(files: &[(&str, &str)]) -> Vault {
        let mut vault = Vault {
            root: PathBuf::from("/vault"),
            kind: Root::Obsidian,
            files: BTreeMap::new(),
            index: Index::default(),
        };
        for (path, text) in files {
            let md = is_note(path);
            vault.files.insert(
                path.to_string(),
                File {
                    note: if md { parse::parse(text) } else { Note::default() },
                    text: md.then(|| text.to_string()),
                    open: None,
                    stamp: None,
                },
            );
        }
        vault.reindex();
        vault
    }

    fn walk(&self) -> Vec<String> {
        let mut builder = ignore::WalkBuilder::new(&self.root);
        builder.hidden(true).require_git(false);
        if self.kind == Root::Obsidian {
            builder.git_ignore(false).git_global(false).git_exclude(false).ignore(false).parents(false);
        }
        let mut out = Vec::new();
        for entry in builder.build().flatten() {
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let Some(rel) = self.rel(entry.path()) else { continue };
            if self.kind == Root::Obsidian || is_note(&rel) {
                out.push(rel);
            }
        }
        out
    }

    fn read(&self, rel: &str) -> File {
        let path = self.path(rel);
        let stamp = std::fs::metadata(&path).ok().map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()));
        let text = is_note(rel).then(|| {
            std::fs::read(&path).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
        });
        File {
            note: text.as_deref().map(parse::parse).unwrap_or_default(),
            text,
            open: None,
            stamp,
        }
    }

    fn reindex(&mut self) {
        self.index = Index::build(self.files.keys().cloned());
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// The path from the root, if `path` is inside the vault.
    pub fn rel(&self, path: &Path) -> Option<String> {
        let rel = path.strip_prefix(&self.root).ok()?;
        let parts: Vec<_> = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect();
        (!parts.is_empty()).then(|| parts.join("/"))
    }

    pub fn contains(&self, path: &Path) -> bool {
        match self.kind {
            Root::Single => self.rel(path).is_some_and(|r| self.files.contains_key(&r)),
            _ => self.rel(path).is_some_and(|r| !r.split('/').any(|p| p.starts_with('.'))),
        }
    }

    pub fn note(&self, rel: &str) -> Option<&Note> {
        self.files.get(rel).filter(|f| f.text.is_some()).map(|f| &f.note)
    }

    pub fn text(&self, rel: &str) -> Option<&str> {
        self.files.get(rel)?.text.as_deref()
    }

    /// Notes, with their parses.
    pub fn notes(&self) -> impl Iterator<Item = (&String, &Note)> {
        self.files.iter().filter(|(_, f)| f.text.is_some()).map(|(p, f)| (p, &f.note))
    }

    pub fn exists(&self, rel: &str) -> bool {
        self.index.lower.contains_key(&rel.to_lowercase())
    }

    pub fn resolve(&self, from: &str, link: &str) -> Dest {
        self.index.resolve(from, link)
    }

    pub fn link_text(&self, rel: &str) -> String {
        self.index.link_text(rel)
    }

    pub fn open(&mut self, rel: &str, text: String, version: i32) {
        let new = !self.files.contains_key(rel);
        let file = self.files.entry(rel.to_string()).or_insert_with(|| File {
            text: None,
            note: Note::default(),
            open: None,
            stamp: None,
        });
        file.note = parse::parse(&text);
        file.text = Some(text);
        file.open = Some(version);
        if new {
            self.reindex();
        }
    }

    pub fn file_mut(&mut self, rel: &str) -> Option<&mut File> {
        self.files.get_mut(rel)
    }

    /// Reparses an open file after its text changed.
    pub fn reparse(&mut self, rel: &str) {
        if let Some(f) = self.files.get_mut(rel) {
            f.note = f.text.as_deref().map(parse::parse).unwrap_or_default();
        }
    }

    /// The editor closed a file: go back to what is on disk.
    pub fn close(&mut self, rel: &str) {
        if self.path(rel).is_file() && (self.kind != Root::Single || self.files.contains_key(rel)) {
            let file = self.read(rel);
            self.files.insert(rel.to_string(), file);
        } else if self.files.remove(rel).is_some() {
            self.reindex();
        }
    }

    /// Brings files not open in the editor in line with the disk. Returns
    /// whether anything changed.
    pub fn rescan(&mut self) -> bool {
        if self.kind == Root::Single {
            return false;
        }
        let on_disk = self.walk();
        let listed: std::collections::HashSet<&String> = on_disk.iter().collect();
        let mut changed = false;
        let gone: Vec<String> = self
            .files
            .iter()
            .filter(|(rel, f)| f.open.is_none() && !listed.contains(rel))
            .map(|(rel, _)| rel.clone())
            .collect();
        for rel in gone {
            self.files.remove(&rel);
            changed = true;
        }
        let mut added = false;
        drop(listed);
        for rel in on_disk {
            let stamp = std::fs::metadata(self.path(&rel))
                .ok()
                .map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()));
            match self.files.get(&rel) {
                Some(f) if f.open.is_some() || f.stamp == stamp => {}
                existing => {
                    added |= existing.is_none();
                    let file = self.read(&rel);
                    self.files.insert(rel, file);
                    changed = true;
                }
            }
        }
        if changed || added {
            self.reindex();
        }
        changed
    }

    /// Where a note not written yet would be created, from the root.
    pub fn create_path(&self, from: &str, link: &str) -> Option<String> {
        let path = normalize(dir(from), link);
        if path.is_empty() || is_attachment(&path) {
            return None;
        }
        Some(if is_note(&path) { path } else { format!("{path}.md") })
    }

    /// Seconds since the epoch the file last changed on disk; 0 if unknown.
    pub fn modified(&self, rel: &str) -> u64 {
        self.files
            .get(rel)
            .and_then(|f| f.stamp)
            .and_then(|(t, _)| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs())
    }

    /// Outside an Obsidian vault only notes are listed; an embedded file is
    /// looked for on disk, from the root and from the note's folder.
    pub fn on_disk(&self, from: &str, link: &str) -> bool {
        self.kind != Root::Obsidian
            && [normalize("", link), normalize(dir(from), &format!("./{link}"))]
                .iter()
                .any(|p| !p.is_empty() && self.path(p).is_file())
    }

    /// Whether `rel` is taken by a file other than `except`.
    pub fn name_taken(&self, rel: &str, except: &str) -> bool {
        self.index.lower.get(&rel.to_lowercase()).is_some_and(|r| r != except)
    }
}

impl Index {
    pub fn build(paths: impl Iterator<Item = String>) -> Index {
        let mut index = Index::default();
        for rel in paths {
            index.lower.insert(rel.to_lowercase(), rel.clone());
            index.names.entry(link_name(&rel).to_lowercase()).or_default().push(rel);
        }
        index
    }

    /// Resolves the path part of a link written in `from`.
    pub fn resolve(&self, from: &str, link: &str) -> Dest {
        if link.is_empty() {
            return Dest::File(from.to_string());
        }
        let path = normalize(dir(from), link);
        let lower = path.to_lowercase();
        let with_md = format!("{lower}.md");
        // Exact path from the root, then relative to the linking note.
        for candidate in [&lower, &with_md] {
            if let Some(rel) = self.lower.get(candidate.as_str()) {
                return Dest::File(rel.clone());
            }
        }
        let here = dir(from).to_lowercase();
        if !here.is_empty() {
            for candidate in [format!("{here}/{lower}"), format!("{here}/{with_md}")] {
                if let Some(rel) = self.lower.get(&candidate) {
                    return Dest::File(rel.clone());
                }
            }
        }
        // By name, matching any folders written as a suffix of the path.
        let name = lower.rsplit('/').next().unwrap_or(&lower);
        let name = name.strip_suffix(".md").unwrap_or(name);
        let suffixes = [format!("/{lower}"), format!("/{with_md}")];
        let mut best: Option<&String> = None;
        for rel in self.names.get(name).into_iter().flatten() {
            let rl = rel.to_lowercase();
            if !(rl == lower || rl == with_md || suffixes.iter().any(|s| rl.ends_with(s.as_str()))) {
                continue;
            }
            best = match best {
                None => Some(rel),
                Some(b) => Some(if rank(rel, &here) < rank(b, &here) { rel } else { b }),
            };
        }
        match best {
            Some(rel) => Dest::File(rel.clone()),
            None => Dest::Unwritten(lower.strip_suffix(".md").unwrap_or(&lower).to_string()),
        }
    }

    /// The text a new link to `rel` uses: the name alone when no other file
    /// shares it, else the full path from the root (Obsidian's default).
    pub fn link_text(&self, rel: &str) -> String {
        let name = link_name(rel);
        if self.names.get(&name.to_lowercase()).is_some_and(|v| v.len() == 1) {
            name.to_string()
        } else if is_note(rel) {
            rel[..rel.len() - 3].to_string()
        } else {
            rel.to_string()
        }
    }

}

/// Orders candidates for a name: same folder, shallowest, alphabetical.
fn rank<'a>(rel: &'a str, here: &str) -> (bool, usize, &'a str) {
    (dir(rel).to_lowercase() != here, rel.matches('/').count(), rel)
}

/// Joins `./` and `../` links to the linking note's folder; other links are
/// from the root.
fn normalize(here: &str, link: &str) -> String {
    let link = link.trim().trim_start_matches('/');
    let relative = link.starts_with("./") || link.starts_with("../");
    let mut parts: Vec<&str> = if relative && !here.is_empty() { here.split('/').collect() } else { Vec::new() };
    for part in link.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

/// The heading a link's subpath names, by index into the note's headings.
pub fn find_heading(note: &Note, link: &Link) -> Option<usize> {
    let segs = link.headings();
    let last = segs.last()?;
    if link.kind == Kind::Markdown {
        let want = slug(&last.text);
        return note.headings.iter().position(|h| slug(&h.text) == want);
    }
    find_heading_path(note, segs)
}

/// `A#B` names `B` under `A`, falling back to any heading `B`.
pub fn find_heading_path(note: &Note, segs: &[Segment]) -> Option<usize> {
    let keys: Vec<String> = segs.iter().map(|s| heading_key(&s.text)).collect();
    let (last, parents) = keys.split_last()?;
    let mut stack: Vec<(u8, String)> = Vec::new();
    let mut fallback = None;
    for (i, h) in note.headings.iter().enumerate() {
        while stack.last().is_some_and(|(level, _)| *level >= h.level) {
            stack.pop();
        }
        let key = heading_key(&h.text);
        if &key == last {
            let mut ancestors = stack.iter().map(|(_, k)| k);
            if parents.iter().all(|p| ancestors.any(|a| a == p)) {
                return Some(i);
            }
            fallback.get_or_insert(i);
        }
        stack.push((h.level, key));
    }
    fallback
}

pub fn find_block(note: &Note, id: &str) -> Option<usize> {
    note.blocks.iter().position(|b| b.id.eq_ignore_ascii_case(id))
}

/// The headings nested under heading `i`, including itself.
pub fn section(note: &Note, i: usize) -> std::ops::Range<usize> {
    let level = note.headings[i].level;
    let end = note.headings[i + 1..].iter().position(|h| h.level <= level).map_or(note.headings.len(), |j| i + 1 + j);
    i..end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault() -> Vault {
        Vault::from_files(&[
            ("Array.md", "# Array\n## C\n### Static Allocation\n## C++\n### Dynamic Allocation\n"),
            ("2026/11/14.md", ""),
            ("2026/10/14.md", ""),
            ("2026/11/16.md", ""),
            ("School/Notes/Array.md", ""),
            ("School/Index.md", ""),
            ("atlas.design/resume.md", ""),
            ("attachments/Screenshot 1.png", ""),
            ("2.71.md", ""),
            ("16.md", ""),
        ])
    }

    fn file(s: &str) -> Dest {
        Dest::File(s.into())
    }

    #[test]
    fn resolution() {
        let v = vault();
        assert_eq!(v.resolve("x.md", "array"), file("Array.md"));
        assert_eq!(v.resolve("x.md", "2026/11/14"), file("2026/11/14.md"));
        assert_eq!(v.resolve("x.md", "11/14"), file("2026/11/14.md"));
        assert_eq!(v.resolve("x.md", "14"), file("2026/10/14.md"));
        assert_eq!(v.resolve("2026/11/16.md", "14"), file("2026/11/14.md"));
        assert_eq!(v.resolve("x.md", "16"), file("16.md"));
        assert_eq!(v.resolve("x.md", "Notes/Array"), file("School/Notes/Array.md"));
        assert_eq!(v.resolve("School/Index.md", "Notes/Array"), file("School/Notes/Array.md"));
        assert_eq!(v.resolve("School/Notes/Array.md", "Array"), file("Array.md"));
        assert_eq!(v.resolve("School/Notes/Array.md", "Index"), file("School/Index.md"));
        assert_eq!(v.resolve("x.md", "Array.md"), file("Array.md"));
        assert_eq!(v.resolve("x.md", "resume"), file("atlas.design/resume.md"));
        assert_eq!(v.resolve("x.md", "Screenshot 1.png"), file("attachments/Screenshot 1.png"));
        assert_eq!(v.resolve("x.md", "2.71"), file("2.71.md"));
        assert_eq!(v.resolve("x.md", "1.41 sketches"), Dest::Unwritten("1.41 sketches".into()));
        assert_eq!(v.resolve("x.md", "2027/01/04"), Dest::Unwritten("2027/01/04".into()));
        assert_eq!(v.resolve("School/Index.md", "./Notes/Array"), file("School/Notes/Array.md"));
        assert_eq!(v.resolve("x.md", ""), file("x.md"));
    }

    #[test]
    fn generated_link_text() {
        let v = vault();
        assert_eq!(v.link_text("2026/11/14.md"), "2026/11/14");
        assert_eq!(v.link_text("2.71.md"), "2.71");
        assert_eq!(v.link_text("Array.md"), "Array");
        assert_eq!(v.link_text("School/Index.md"), "Index");
        assert_eq!(v.link_text("16.md"), "16");
        assert_eq!(v.link_text("attachments/Screenshot 1.png"), "Screenshot 1.png");
    }

    #[test]
    fn creation_paths() {
        let v = vault();
        assert_eq!(v.create_path("2026/11/16.md", "Dynamic Call").as_deref(), Some("Dynamic Call.md"));
        assert_eq!(v.create_path("x.md", "2027/01/04").as_deref(), Some("2027/01/04.md"));
        assert_eq!(v.create_path("x.md", "missing.png"), None);
    }

    #[test]
    fn headings() {
        let v = vault();
        let note = v.note("Array.md").unwrap();
        let link = |s: &str| parse::parse(s).links.remove(0);
        let text = |l: &str| find_heading(note, &link(l)).map(|i| note.headings[i].text.clone());
        assert_eq!(text("[[Array#C++#Dynamic Allocation]]").as_deref(), Some("Dynamic Allocation"));
        assert_eq!(text("[[Array#C#Static Allocation]]").as_deref(), Some("Static Allocation"));
        assert_eq!(text("[[Array#c++]]").as_deref(), Some("C++"));
        assert_eq!(text("[[Array#C]]").as_deref(), Some("C"));
        assert_eq!(text("[[Array#Dynamic Allocation]]").as_deref(), Some("Dynamic Allocation"));
        assert_eq!(text("[[Array#C#Dynamic Allocation]]").as_deref(), Some("Dynamic Allocation"));
        assert_eq!(text("[[Array#Nope]]"), None);
        assert_eq!(section(note, 1), 1..3);
    }

    #[test]
    fn heading_keys() {
        assert_eq!(heading_key("Why: a  Question?"), "why a question?");
        assert_ne!(heading_key("C"), heading_key("C++"));
    }
}
