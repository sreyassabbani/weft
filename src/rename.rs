//! Rename notes, headings, block ids and notes not written yet, and keep
//! links working when files move (`workspace/willRenameFiles`, Helix's
//! `:move`).
//!
//! Moving files is one function: given the moves, every wikilink must
//! resolve afterward to the file it resolved to before. Links that would not
//! are rewritten in the form Obsidian generates; links that still resolve are
//! left alone.

use std::collections::{BTreeMap, HashMap};

use lsp_types::{
    DocumentChangeOperation, DocumentChanges, OneOf, OptionalVersionedTextDocumentIdentifier, Position,
    PrepareRenameResponse, RenameFile, RenameFileOptions, ResourceOp, TextDocumentEdit, TextEdit, Url,
    WorkspaceEdit,
};

use crate::ctx::Ctx;
use crate::parse::Kind;
use crate::target::{self, On, Part, Target};
use crate::text::Span;
use crate::vault::{self, Dest, Index, Vault};

/// Characters that break a wikilink, so a name may not contain them.
const LINK_BREAKING: &[char] = &['[', ']', '#', '^', '|', '\\', ':'];

/// What a rename at the cursor changes, the range shown, and its old name.
struct Renaming {
    target: Target,
    span: Span,
    old: String,
}

fn renaming(cx: &Ctx, rel: &str, pos: Position) -> Result<Renaming, String> {
    let (line, byte) = cx.byte_of(rel, pos);
    let note = cx.vault.note(rel).ok_or("Not a note")?;
    let here = Span { line, start: byte, end: byte };
    Ok(match target::at(note, line, byte) {
        On::Link(link, _) if link.kind == Kind::Markdown => return Err("Markdown links are not renamed".into()),
        On::Link(link, part) => match target::of_link(cx.vault, rel, link, part) {
            Target::Note(_) if part == Part::Sub => {
                let what = if link.block().is_some() { "block" } else { "heading" };
                return Err(format!("No such {what} to rename"));
            }
            Target::Note(dest) => {
                let old = match &dest {
                    Dest::File(f) => vault::link_name(f).to_string(),
                    Dest::Unwritten(_) => link.path.rsplit('/').next().unwrap_or(&link.path).to_string(),
                };
                if link.path.is_empty() {
                    return Err("Nothing to rename here".into());
                }
                Renaming { target: Target::Note(dest), span: link.path_span, old }
            }
            Target::Heading { file, index } => {
                // The segment under the cursor: `A` in `[[Note#A#B]]` is `A`.
                let segs = link.headings();
                let k = segs.iter().position(|s| byte <= s.span.end).unwrap_or(segs.len() - 1);
                let index = if k + 1 == segs.len() {
                    index
                } else {
                    let n = cx.vault.note(&file).ok_or("No such note")?;
                    vault::find_heading_path(n, &segs[..=k]).ok_or("No such heading to rename")?
                };
                let seg = &segs[k];
                Renaming { old: seg.text.clone(), span: seg.span, target: Target::Heading { file, index } }
            }
            t @ Target::Block { .. } => {
                let seg = link.block().unwrap();
                Renaming { old: seg.text.clone(), span: seg.span, target: t }
            }
        },
        On::Heading(index) => {
            let h = &note.headings[index];
            Renaming { target: Target::Heading { file: rel.into(), index }, span: h.span, old: h.text.clone() }
        }
        On::Block(index) => {
            let b = &note.blocks[index];
            Renaming { target: Target::Block { file: rel.into(), index }, span: b.span, old: b.id.clone() }
        }
        On::Body => Renaming {
            target: Target::Note(Dest::File(rel.into())),
            span: here,
            old: vault::link_name(rel).to_string(),
        },
    })
}

pub fn prepare(cx: &Ctx, rel: &str, pos: Position) -> Result<PrepareRenameResponse, String> {
    let r = renaming(cx, rel, pos)?;
    Ok(PrepareRenameResponse::RangeWithPlaceholder { range: cx.range(rel, r.span), placeholder: r.old })
}

/// The edit, and the file move it contains, if any.
pub fn rename(cx: &Ctx, rel: &str, pos: Position, new: &str) -> Result<(WorkspaceEdit, Option<(String, String)>), String> {
    let r = renaming(cx, rel, pos)?;
    let new = new.trim();
    if new.is_empty() {
        return Err("The new name is empty".into());
    }
    if new == r.old {
        return Ok((WorkspaceEdit::default(), None));
    }
    let vault = cx.vault;
    let mut edits: BTreeMap<String, Vec<(Span, String)>> = BTreeMap::new();
    let mut moved = None;
    match &r.target {
        Target::Note(Dest::File(file)) => {
            let to = destination(vault, file, new)?;
            if &to == file {
                return Ok((WorkspaceEdit::default(), None));
            }
            edits = move_edits(vault, &[(file.clone(), to.clone())]);
            moved = Some((file.clone(), to));
        }
        Target::Note(dest @ Dest::Unwritten(_)) => {
            check_name(new)?;
            for (from, note) in vault.notes() {
                for link in note.links.iter().filter(|l| l.kind == Kind::Wiki) {
                    if &vault.resolve(from, &link.path) == dest {
                        edits.entry(from.clone()).or_default().push((link.path_span, new.to_string()));
                    }
                }
            }
        }
        Target::Heading { file, index } => {
            check_name(new)?;
            let note = vault.note(file).ok_or("No such note")?;
            edits.entry(file.clone()).or_default().push((note.headings[*index].span, new.to_string()));
            let section = vault::section(note, *index);
            let old_key = vault::heading_key(&note.headings[*index].text);
            for (from, links) in vault.notes() {
                for link in links.links.iter().filter(|l| l.kind == Kind::Wiki && l.block().is_none()) {
                    if vault.resolve(from, &link.path) != Dest::File(file.clone()) {
                        continue;
                    }
                    if !vault::find_heading(note, link).is_some_and(|i| section.contains(&i)) {
                        continue;
                    }
                    for seg in link.headings().iter().filter(|s| vault::heading_key(&s.text) == old_key) {
                        edits.entry(from.clone()).or_default().push((seg.span, new.to_string()));
                    }
                }
            }
        }
        Target::Block { file, index } => {
            if !new.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                return Err("A block id has only letters, digits and dashes".into());
            }
            let note = vault.note(file).ok_or("No such note")?;
            if vault::find_block(note, new).is_some() {
                return Err(format!("“^{new}” is already used in this note"));
            }
            edits.entry(file.clone()).or_default().push((note.blocks[*index].span, new.to_string()));
            for r in target::links_to(vault, &r.target) {
                edits.entry(r.file.clone()).or_default().push((r.link.block().unwrap().span, new.to_string()));
            }
        }
    }
    let mut edit = workspace_edit(cx, edits);
    if let Some((from, to)) = &moved {
        let ops = match edit.document_changes.get_or_insert(DocumentChanges::Operations(Vec::new())) {
            DocumentChanges::Operations(ops) => ops,
            DocumentChanges::Edits(_) => unreachable!(),
        };
        ops.push(DocumentChangeOperation::Op(ResourceOp::Rename(RenameFile {
            old_uri: cx.uri(from),
            new_uri: cx.uri(to),
            options: Some(RenameFileOptions { overwrite: Some(false), ignore_if_exists: Some(false) }),
            annotation_id: None,
        })));
    }
    Ok((edit, moved))
}

fn check_name(new: &str) -> Result<(), String> {
    match new.chars().find(|c| LINK_BREAKING.contains(c)) {
        Some(c) => Err(format!("Names cannot contain “{c}”")),
        None => Ok(()),
    }
}

/// The new path of `file` renamed to `new`: in the same folder, or from the
/// root when `new` has a `/`.
fn destination(vault: &Vault, file: &str, new: &str) -> Result<String, String> {
    let new = new.trim_matches('/');
    let name = new.rsplit('/').next().unwrap_or(new);
    check_name(new)?;
    if name.starts_with('.') || name.is_empty() {
        return Err("The name cannot be empty or start with “.”".into());
    }
    let mut to = if new.contains('/') {
        new.to_string()
    } else {
        match file.rfind('/') {
            Some(i) => format!("{}/{new}", &file[..i]),
            None => new.to_string(),
        }
    };
    if vault::is_note(file) {
        if !vault::is_note(&to) {
            to += ".md";
        }
    } else if let Some((_, ext)) = file.rsplit_once('.') {
        if !to.to_lowercase().ends_with(&format!(".{}", ext.to_lowercase())) {
            to = format!("{to}.{ext}");
        }
    }
    let case_only = to.to_lowercase() == file.to_lowercase();
    if !case_only && (vault.name_taken(&to, file) || vault.path(&to).exists()) {
        return Err(format!("“{to}” already exists"));
    }
    Ok(to)
}

/// Edits that keep every wikilink pointing at the same file after `moves`.
pub fn move_edits(vault: &Vault, moves: &[(String, String)]) -> BTreeMap<String, Vec<(Span, String)>> {
    let moved: HashMap<&str, &str> = moves.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let after = Index::build(vault.files.keys().map(|k| moved.get(k.as_str()).map_or(k.clone(), |n| n.to_string())));
    let mut edits: BTreeMap<String, Vec<(Span, String)>> = BTreeMap::new();
    for (from, note) in vault.notes() {
        let from_after = moved.get(from.as_str()).copied().unwrap_or(from);
        for link in note.links.iter().filter(|l| l.kind == Kind::Wiki && !l.path.is_empty()) {
            let Dest::File(target) = vault.resolve(from, &link.path) else { continue };
            let want = moved.get(target.as_str()).map_or(target.clone(), |n| n.to_string());
            if after.resolve(from_after, &link.path) == Dest::File(want.clone()) {
                continue;
            }
            edits.entry(from.clone()).or_default().push((link.path_span, after.link_text(&want)));
        }
    }
    edits
}

/// Text edits addressed to each file as it is now, versioned when open.
pub fn workspace_edit(cx: &Ctx, edits: BTreeMap<String, Vec<(Span, String)>>) -> WorkspaceEdit {
    if edits.is_empty() {
        return WorkspaceEdit::default();
    }
    let ops = edits
        .into_iter()
        .map(|(file, mut list)| {
            list.sort_by_key(|(s, _)| (s.line, s.start));
            list.dedup_by_key(|(s, _)| (s.line, s.start));
            let version = cx.vault.files.get(&file).and_then(|f| f.open);
            DocumentChangeOperation::Edit(TextDocumentEdit {
                text_document: OptionalVersionedTextDocumentIdentifier { uri: cx.uri(&file), version },
                edits: list.into_iter().map(|(s, t)| OneOf::Left(TextEdit::new(cx.range(&file, s), t))).collect(),
            })
        })
        .collect();
    WorkspaceEdit { document_changes: Some(DocumentChanges::Operations(ops)), ..Default::default() }
}

/// `workspace/willRenameFiles`: the moves are about to happen.
pub fn will_rename(cx: &Ctx, files: &[(Url, Url)]) -> WorkspaceEdit {
    let vault = cx.vault;
    let mut moves = Vec::new();
    for (old, new) in files {
        let (Ok(old), Ok(new)) = (old.to_file_path(), new.to_file_path()) else { continue };
        let (Some(from), Some(to)) = (vault.rel(&old), vault.rel(&new)) else { continue };
        if vault.files.contains_key(&from) {
            moves.push((from, to));
        } else {
            // A folder: everything in it moves.
            let prefix = format!("{from}/");
            for rel in vault.files.keys().filter(|k| k.starts_with(&prefix)) {
                moves.push((rel.clone(), format!("{to}/{}", &rel[prefix.len()..])));
            }
        }
    }
    workspace_edit(cx, move_edits(vault, &moves))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::Encoding;

    fn vault() -> Vault {
        Vault::from_files(&[
            ("Array.md", "# Array\n## C++\n### Dynamic Allocation\nend ^blk\n"),
            ("Sorting.md", "[[Array#C++#Dynamic Allocation|vectors]] [[array]] [[Array#^blk]]\n[[Dynamic Call]] [[#Local]]\n## Local"),
            ("Other.md", "[[Dynamic Call|lb]] [[Array#C++]] [[2026/11/14]] [[Sorting]]"),
            ("2026/11/14.md", "[[Other]]"),
            ("2026/10/14.md", ""),
        ])
    }

    fn changes(edit: &WorkspaceEdit) -> Vec<(String, String)> {
        let Some(DocumentChanges::Operations(ops)) = &edit.document_changes else { return Vec::new() };
        let mut out = Vec::new();
        for op in ops {
            match op {
                DocumentChangeOperation::Edit(e) => {
                    let file = e.text_document.uri.path().rsplit('/').next().unwrap().to_string();
                    for t in &e.edits {
                        let OneOf::Left(t) = t else { unreachable!() };
                        out.push((file.clone(), t.new_text.clone()));
                    }
                }
                DocumentChangeOperation::Op(ResourceOp::Rename(r)) => {
                    out.push(("move".into(), r.new_uri.path().to_string()));
                }
                _ => {}
            }
        }
        out
    }

    fn run(v: &Vault, rel: &str, line: u32, col: u32, new: &str) -> Result<Vec<(String, String)>, String> {
        let cx = Ctx { vault: v, enc: Encoding::Utf8 };
        rename(&cx, rel, Position::new(line, col), new).map(|(e, _)| changes(&e))
    }

    #[test]
    fn note() {
        let v = vault();
        let got = run(&v, "Sorting.md", 0, 3, "Arrays").unwrap();
        assert_eq!(got.len(), 5);
        assert!(got.iter().filter(|(f, _)| f != "move").all(|(_, t)| t == "Arrays"));
        assert_eq!(got.last().unwrap().1, "/vault/Arrays.md");
        // From inside the note itself.
        let got = run(&v, "Array.md", 3, 1, "Arrays").unwrap();
        assert_eq!(got.len(), 5);
    }

    #[test]
    fn refusals() {
        let v = vault();
        assert!(run(&v, "Sorting.md", 0, 3, "Other").unwrap_err().contains("already exists"));
        assert!(run(&v, "Sorting.md", 0, 3, "a#b").is_err());
        assert!(run(&v, "Sorting.md", 0, 3, "ARRAY").is_ok());
    }

    #[test]
    fn heading_and_block() {
        let v = vault();
        // On `Dynamic Allocation`, then on `C++`, in `[[Array#C++#Dynamic Allocation|vectors]]`.
        let got = run(&v, "Sorting.md", 0, 15, "Vectors").unwrap();
        assert_eq!(got, [("Array.md".into(), "Vectors".into()), ("Sorting.md".into(), "Vectors".into())]);
        let got = run(&v, "Sorting.md", 0, 9, "Cpp").unwrap();
        assert_eq!(got.len(), 3, "{got:?}");
        let got = run(&v, "Array.md", 1, 4, "Cpp").unwrap();
        assert_eq!(got.len(), 3, "{got:?}");
        let got = run(&v, "Array.md", 3, 6, "b2").unwrap();
        assert_eq!(got, [("Array.md".into(), "b2".into()), ("Sorting.md".into(), "b2".into())]);
        let got = run(&v, "Sorting.md", 1, 21, "Here").unwrap();
        assert_eq!(got, [("Sorting.md".into(), "Here".into()), ("Sorting.md".into(), "Here".into())]);
    }

    #[test]
    fn unwritten() {
        let v = vault();
        let got = run(&v, "Sorting.md", 1, 4, "Dynamic call (CS)").unwrap();
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn moves_keep_links() {
        let v = vault();
        // A daily note moving next to another of the same name.
        let edits = move_edits(&v, &[("2026/11/14.md".into(), "2026/10/archive/14.md".into())]);
        let texts: Vec<&str> = edits.values().flatten().map(|(_, t)| t.as_str()).collect();
        assert_eq!(texts, ["2026/10/archive/14"]);
        // A note moved into a folder keeps a unique name: nothing to rewrite.
        assert!(move_edits(&v, &[("Sorting.md".into(), "School/Sorting.md".into())]).is_empty());
        // Links from the moved note keep working.
        let edits = move_edits(&v, &[("Other.md".into(), "deep/Other.md".into())]);
        assert!(edits.is_empty(), "{edits:?}");
    }
}
