//! `[[` completion: notes, aliases, notes not written yet, attachments in
//! embeds, and headings or blocks after `#`.
//!
//! Helix filters the menu by the word before the cursor and closes it on any
//! other character. Titles are sentences, so the server does the filtering on
//! the whole text after `[[`, answers incomplete lists so Helix asks again on
//! every keystroke, and asks to be triggered on spaces and punctuation.

use std::collections::BTreeMap;

use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionList, CompletionTextEdit, InsertReplaceEdit, Position, Range,
};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::ctx::Ctx;
use crate::text::{Span, split_lines};
use crate::vault::{self, Dest};

const LIMIT: usize = 50;

/// Characters that keep the menu alive inside a link: Helix closes it on
/// anything that is not alphanumeric or `_`.
pub const TRIGGERS: &[&str] = &[
    "[", " ", "!", "\"", "#", "$", "%", "&", "'", "(", ")", "*", "+", ",", "-", ".", "/", ":", ";",
    "<", "=", ">", "?", "@", "\\", "^", "`", "{", "}", "~", "’", "‘", "“", "”", "–", "—", "…",
];

struct Candidate {
    label: String,
    insert: String,
    /// What the query is matched against.
    hay: String,
    detail: Option<String>,
    kind: CompletionItemKind,
    /// Tie-break for an empty query: most recently changed first.
    recency: u64,
}

pub fn complete(cx: &Ctx, rel: &str, pos: Position) -> Option<CompletionList> {
    let text = cx.vault.text(rel)?;
    let (line_no, byte) = cx.byte_of(rel, pos);
    let line = *split_lines(text).get(line_no as usize)?;
    let byte = byte as usize;
    let before = &line[..byte];
    let open = before.rfind("[[")?;
    let query = &before[open + 2..];
    if query.contains("]]") || query.contains('|') {
        return None;
    }
    let embed = open > 0 && line.as_bytes()[open - 1] == b'!';

    // Helix moves the cursor to the start of an edit that reaches past it, so
    // the edit inserts up to the cursor. In replace mode it also replaces the
    // rest of the name, up to an alias or heading part or the closing `]]`.
    // Closing brackets are added only when missing: Helix auto-pairs them.
    let after = &line[byte..];
    let close = after.find("]]").filter(|&c| !after[..c].contains("[["));
    let stop: &[char] = if query.contains('#') { &['|'] } else { &['|', '#'] };
    let (end, closing) = match close {
        Some(c) => (byte + after[..c].find(stop).unwrap_or(c), ""),
        None => (byte, "]]"),
    };
    let line_no = line_no as usize;
    let ranges = (cx.range(rel, Span::new(line_no, open + 2, byte)), cx.range(rel, Span::new(line_no, open + 2, end)));

    let (needle, candidates) = match query.find('#') {
        Some(h) => (query.rsplit('#').next().unwrap_or(""), sub_candidates(cx, rel, &query[..h], &query[h + 1..])?),
        None => (query, note_candidates(cx, embed, rel, line_no as u32, open as u32)),
    };
    // A finished link: Helix asks again after accepting an item when the
    // name ends in punctuation; there is nothing left to offer.
    if close.is_some() && candidates.iter().any(|c| c.insert == query) {
        return None;
    }
    Some(CompletionList { is_incomplete: true, items: rank(needle, query, candidates, ranges, closing) })
}

/// `at` is where the link being typed starts; it is not a note of its own.
fn note_candidates(cx: &Ctx, embed: bool, current: &str, line: u32, at: u32) -> Vec<Candidate> {
    let vault = cx.vault;
    let mut out = Vec::new();
    for (rel, file) in &vault.files {
        let is_note = vault::is_note(rel);
        if !is_note && !embed {
            continue;
        }
        let link = vault.link_text(rel);
        let folder = rel.rfind('/').map(|i| rel[..i].to_string());
        out.push(Candidate {
            label: link.clone(),
            insert: link.clone(),
            hay: link.clone(),
            detail: folder.filter(|_| !link.contains('/')),
            kind: if is_note { CompletionItemKind::FILE } else { CompletionItemKind::FOLDER },
            recency: vault.modified(rel),
        });
        for alias in &file.note.aliases {
            out.push(Candidate {
                label: alias.clone(),
                insert: format!("{link}|{alias}"),
                hay: alias.clone(),
                detail: Some(format!("→ {link}")),
                kind: CompletionItemKind::REFERENCE,
                recency: vault.modified(rel),
            });
        }
    }
    // Notes not written yet, spelled as first written.
    let mut unwritten: BTreeMap<String, String> = BTreeMap::new();
    for (from, note) in vault.notes() {
        for link in &note.links {
            if link.kind != crate::parse::Kind::Wiki || link.path.is_empty() || vault::is_attachment(&link.path) {
                continue;
            }
            if from == current && link.span.line == line && link.span.contains(line, at + link.embed as u32) {
                continue;
            }
            if let Dest::Unwritten(key) = vault.resolve(from, &link.path) {
                unwritten.entry(key).or_insert_with(|| link.path.clone());
            }
        }
    }
    for name in unwritten.into_values() {
        out.push(Candidate {
            label: name.clone(),
            insert: name.clone(),
            hay: name,
            detail: Some("not written yet".into()),
            kind: CompletionItemKind::TEXT,
            recency: 0,
        });
    }
    out
}

/// Headings (`[[Note#`) or block ids (`[[Note#^`) of the note being linked.
fn sub_candidates(cx: &Ctx, rel: &str, path: &str, sub: &str) -> Option<Vec<Candidate>> {
    let Dest::File(file) = cx.vault.resolve(rel, path) else { return Some(Vec::new()) };
    let note = cx.vault.note(&file)?;
    let blocks = sub.rsplit('#').next().is_some_and(|s| s.starts_with('^'));
    let out = if blocks {
        note.blocks
            .iter()
            .map(|b| Candidate {
                label: format!("^{}", b.id),
                insert: format!("{path}#^{}", b.id),
                hay: format!("^{}", b.id),
                detail: None,
                kind: CompletionItemKind::REFERENCE,
                recency: 0,
            })
            .collect()
    } else {
        note.headings
            .iter()
            .filter(|h| !h.text.is_empty())
            .map(|h| Candidate {
                label: h.text.clone(),
                insert: format!("{path}#{}", h.text),
                hay: h.text.clone(),
                detail: Some("#".repeat(h.level as usize)),
                kind: CompletionItemKind::REFERENCE,
                recency: 0,
            })
            .collect()
    };
    Some(out)
}

fn rank(
    needle: &str,
    query: &str,
    candidates: Vec<Candidate>,
    (insert, replace): (Range, Range),
    closing: &str,
) -> Vec<CompletionItem> {
    let pattern = Pattern::parse(needle, CaseMatching::Ignore, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, Candidate)> = candidates
        .into_iter()
        .filter_map(|c| {
            let score = if needle.trim().is_empty() {
                0
            } else {
                pattern.score(Utf32Str::new(&c.hay, &mut buf), &mut matcher)?
            };
            Some((score, c))
        })
        .collect();
    scored.sort_by(|(sa, a), (sb, b)| {
        sb.cmp(sa)
            .then(b.recency.cmp(&a.recency))
            .then(a.label.len().cmp(&b.label.len()))
            .then_with(|| a.label.cmp(&b.label))
    });
    scored
        .into_iter()
        .take(LIMIT)
        .enumerate()
        .map(|(i, (_, c))| CompletionItem {
            // Helix scores its own filter (the word before the cursor) against
            // this. Starting with the query makes every item score alike, so
            // the order below holds.
            filter_text: Some(format!("{query} {}", c.label)),
            sort_text: Some(format!("{i:04}")),
            text_edit: Some(CompletionTextEdit::InsertAndReplace(InsertReplaceEdit {
                new_text: format!("{}{closing}", c.insert),
                insert,
                replace,
            })),
            label: c.label,
            detail: c.detail,
            kind: Some(c.kind),
            ..Default::default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::Encoding;
    use crate::vault::Vault;

    fn vault(current: &str) -> Vault {
        Vault::from_files(&[
            ("Can combinatorics be explained entirely physically?.md", ""),
            ("Combinatorics.md", "# Combinatorics\n## Hard puzzles\npara ^p1\n"),
            ("Aren't we just going round?.md", ""),
            ("2026/11/14.md", ""),
            ("2026/10/14.md", ""),
            ("Directional Edges.md", "---\naliases:\n  - Neighbour Graphs\n---\n"),
            ("attachments/Weather.png", ""),
            ("x.md", current),
        ])
    }

    /// (label, new text, insert range, replace range)
    fn items(current: &str, col: u32) -> Vec<(String, String, Range, Range)> {
        let v = vault(current);
        let cx = Ctx { vault: &v, enc: Encoding::Utf8 };
        let list = complete(&cx, "x.md", Position::new(0, col)).map(|l| l.items).unwrap_or_default();
        list.into_iter()
            .map(|i| match i.text_edit {
                Some(CompletionTextEdit::InsertAndReplace(e)) => (i.label, e.new_text, e.insert, e.replace),
                _ => unreachable!(),
            })
            .collect()
    }

    #[test]
    fn multi_word_titles() {
        let text = "see [[combi phys";
        let got = items(text, text.len() as u32);
        assert_eq!(got[0].1, "Can combinatorics be explained entirely physically?]]");
        assert_eq!(got[0].2, Range::new(Position::new(0, 6), Position::new(0, 16)));
        assert_eq!(got[0].3, got[0].2);
        let text = "[[Aren't we";
        assert_eq!(items(text, text.len() as u32)[0].0, "Aren't we just going round?");
    }

    #[test]
    fn closing_brackets_and_aliases() {
        // Helix auto-pairs brackets: `[[]]` with the cursor in the middle.
        let got = items("[[Combi]]", 7);
        assert_eq!(got[0].1, "Combinatorics");
        assert_eq!((got[0].2.end, got[0].3.end), (Position::new(0, 7), Position::new(0, 7)));
        // In replace mode the rest of the name goes; an alias stays.
        let got = items("[[Combi|the hard one]]", 4);
        assert_eq!(got[0].1, "Combinatorics");
        assert_eq!((got[0].2.end, got[0].3.end), (Position::new(0, 4), Position::new(0, 7)));
        // After accepting, Helix asks again; a finished name gets nothing.
        assert!(items("[[Aren't we just going round?]]", 29).is_empty());
        let got = items("[[Neighbour", 11);
        assert_eq!(got[0].1, "Directional Edges|Neighbour Graphs]]");
    }

    #[test]
    fn daily_notes_and_embeds() {
        let got = items("[[11/14", 7);
        assert_eq!(got[0].1, "2026/11/14]]");
        assert!(items("[[Weat", 6).iter().all(|i| i.0 != "Weather.png"));
        assert_eq!(items("![[Weat", 7)[0].1, "Weather.png]]");
    }

    #[test]
    fn headings_and_blocks() {
        let got = items("[[Combinatorics#hard", 20);
        assert_eq!(got[0].1, "Combinatorics#Hard puzzles]]");
        let got = items("[[Combinatorics#^", 17);
        assert_eq!(got[0].1, "Combinatorics#^p1]]");
        assert!(items("text [[a]] more", 15).is_empty());
        assert!(items("[[Combi|al", 10).is_empty());
    }

    #[test]
    fn unwritten_notes_are_offered() {
        let text = "[[Dynamic Call]] [[Dyna";
        let got = items(text, text.len() as u32);
        assert_eq!(got[0].0, "Dynamic Call");
    }
}
