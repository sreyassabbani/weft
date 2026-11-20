//! Checks against a real vault, run when `WEFT_TEST_VAULT` names a copy of
//! one (never the vault itself: nothing is written, but there is no reason
//! to point tests at live notes).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use weft::ctx::Ctx;
use weft::parse::Kind;
use weft::text::{self, Encoding};
use weft::vault::{self, Dest, Vault};

fn load() -> Option<Vault> {
    let root = PathBuf::from(std::env::var_os("WEFT_TEST_VAULT")?);
    // Found the way the server finds it, from a note at the root.
    let (found, kind) = vault::discover(&root.join("note.md"));
    assert_eq!(found, root, "WEFT_TEST_VAULT is not the root of a vault");
    Some(Vault::load(root, kind, None))
}

/// Every wikilink, by its note and its place in it, as (link text, where it
/// resolves).
type Resolutions = BTreeMap<(String, usize), (String, Dest)>;

fn resolutions(v: &Vault) -> Resolutions {
    let mut out = BTreeMap::new();
    for (from, note) in v.notes() {
        for (i, link) in note.links.iter().filter(|l| l.kind == Kind::Wiki).enumerate() {
            out.insert((from.clone(), i), (link.path.clone(), v.resolve(from, &link.path)));
        }
    }
    out
}

/// The vault's files with `edits` applied and `moves` made.
fn apply(v: &Vault, edits: &BTreeMap<String, Vec<(text::Span, String)>>, moves: &[(String, String)]) -> Vault {
    let mut files: Vec<(String, String)> = Vec::new();
    for (rel, file) in &v.files {
        let mut text = file.text.clone().unwrap_or_default();
        if let Some(list) = edits.get(rel) {
            let mut list = list.clone();
            list.sort_by_key(|(s, _)| std::cmp::Reverse((s.line, s.start)));
            for (span, new) in list {
                let start = line_offset(&text, span.line) + span.start as usize;
                let end = line_offset(&text, span.line) + span.end as usize;
                text.replace_range(start..end, &new);
            }
        }
        let rel = moves.iter().find(|(a, _)| a == rel).map_or(rel.clone(), |(_, b)| b.clone());
        files.push((rel, text));
    }
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    Vault::from_files(&refs)
}

fn line_offset(text: &str, line: u32) -> usize {
    text.split_inclusive('\n').take(line as usize).map(str::len).sum()
}

#[test]
fn loads_and_resolves() {
    let Some(v) = load() else { return };
    let start = Instant::now();
    let all = resolutions(&v);
    println!("{} files, {} wikilinks resolved in {:?}", v.files.len(), all.len(), start.elapsed());
    let mut unwritten: Vec<&str> =
        all.values().filter_map(|(_, d)| if let Dest::Unwritten(k) = d { Some(k.as_str()) } else { None }).collect();
    unwritten.sort();
    unwritten.dedup();
    println!("{} notes not written yet", unwritten.len());

    for ((from, _), (path, dest)) in &all {
        // A link written as a path from the root names exactly that file.
        if path.contains('/') && !path.starts_with('.') {
            let exact = format!("{path}.md");
            if v.files.contains_key(&exact) {
                assert_eq!(dest, &Dest::File(exact), "{from}: [[{path}]]");
            }
        }
        // A name shared by no other file resolves to that file.
        if let Dest::File(f) = dest {
            assert!(vault::link_name(f).eq_ignore_ascii_case(path.rsplit('/').next().unwrap().trim_end_matches(".md"))
                || vault::link_name(f).eq_ignore_ascii_case(path.rsplit('/').next().unwrap()),
                "{from}: [[{path}]] -> {f}");
        }
    }

    // Every heading link in an existing note names a heading with that name.
    let cx = Ctx { vault: &v, enc: Encoding::Utf8 };
    for (from, note) in v.notes() {
        for link in note.links.iter().filter(|l| l.kind == Kind::Wiki && !l.headings().is_empty()) {
            let Dest::File(f) = v.resolve(from, &link.path) else { continue };
            let target = v.note(&f).unwrap();
            // A heading that is not there is reported below.
            let Some(i) = vault::find_heading(target, link) else { continue };
            let want = vault::heading_key(&link.headings().last().unwrap().text);
            assert_eq!(vault::heading_key(&target.headings[i].text), want);
        }
        let diags = weft::diagnose::diagnose(&cx, from);
        for d in diags {
            println!("diagnostic in {from}: {}", d.message);
        }
    }
}

#[test]
fn renaming_any_note_keeps_every_link() {
    let Some(v) = load() else { return };
    let before = resolutions(&v);
    let start = Instant::now();
    let notes: Vec<String> = v.notes().map(|(r, _)| r.clone()).collect();
    for rel in &notes {
        let to = match rel.rfind('/') {
            Some(i) => format!("{}/{} (renamed).md", &rel[..i], &vault::link_name(rel)),
            None => format!("{} (renamed).md", vault::link_name(rel)),
        };
        check_move(&v, &before, &[(rel.clone(), to)]);
    }
    println!("renamed {} notes in {:?}", notes.len(), start.elapsed());
}

#[test]
fn moving_daily_notes_keeps_every_link() {
    let Some(v) = load() else { return };
    let before = resolutions(&v);
    // Moving a note next to others of the same name, and to the root, where
    // a bare name would now find it first.
    let daily: Vec<String> = v
        .files
        .keys()
        .filter(|r| r.matches('/').count() == 2 && vault::is_note(r))
        .cloned()
        .collect();
    for rel in daily.iter().take(40) {
        let name = rel.rsplit('/').next().unwrap();
        check_move(&v, &before, &[(rel.clone(), format!("archive/{name}"))]);
        if !v.files.contains_key(name) {
            check_move(&v, &before, &[(rel.clone(), name.to_string())]);
        }
    }
}

fn check_move(v: &Vault, before: &Resolutions, moves: &[(String, String)]) {
    let edits = weft::rename::move_edits(v, moves);
    let after_vault = apply(v, &edits, moves);
    let map = |r: &str| moves.iter().find(|(a, _)| a == r).map_or(r.to_string(), |(_, b)| b.clone());
    let after = resolutions(&after_vault);
    assert_eq!(before.len(), after.len());
    for ((from, i), (path, dest)) in before {
        let (new_path, new_dest) = &after[&(map(from), *i)];
        if let Dest::File(f) = dest {
            assert_eq!(new_dest, &Dest::File(map(f)), "{moves:?}: [[{path}]] in {from} became [[{new_path}]]");
        }
    }
}

#[test]
fn renaming_linked_headings_keeps_their_links() {
    let Some(v) = load() else { return };
    let cx = Ctx { vault: &v, enc: Encoding::Utf8 };
    let mut checked = 0;
    for (rel, note) in v.notes() {
        for (index, h) in note.headings.iter().enumerate() {
            let target = weft::target::Target::Heading { file: rel.clone(), index };
            let refs = weft::target::links_to(&v, &target);
            if refs.is_empty() {
                continue;
            }
            let pos = lsp_types::Position::new(h.span.line, h.span.start);
            let (edit, _) = weft::rename::rename(&cx, rel, pos, &format!("{} renamed", h.text)).unwrap();
            let edits = spans(&v, &edit);
            let after = apply(&v, &edits, &[]);
            let renamed = weft::target::links_to(&after, &target);
            assert_eq!(refs.len(), renamed.len(), "heading {} in {rel}", h.text);
            checked += 1;
        }
    }
    println!("renamed {checked} linked headings");
}

/// A workspace edit as spans, for `apply`.
fn spans(v: &Vault, edit: &lsp_types::WorkspaceEdit) -> BTreeMap<String, Vec<(text::Span, String)>> {
    let mut out: BTreeMap<String, Vec<(text::Span, String)>> = BTreeMap::new();
    let Some(lsp_types::DocumentChanges::Operations(ops)) = &edit.document_changes else { return out };
    for op in ops {
        let lsp_types::DocumentChangeOperation::Edit(e) = op else { continue };
        let rel = v.rel(&e.text_document.uri.to_file_path().unwrap()).unwrap();
        for t in &e.edits {
            let lsp_types::OneOf::Left(t) = t else { continue };
            let span = text::Span { line: t.range.start.line, start: t.range.start.character, end: t.range.end.character };
            out.entry(rel.clone()).or_default().push((span, t.new_text.clone()));
        }
    }
    out
}
