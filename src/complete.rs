//! `[[` completion: notes, aliases, notes not written yet, attachments in
//! embeds, and headings or blocks after `#`. Also `@` for daily notes, which
//! inserts a plain link (see `dates`).
//!
//! Helix filters the menu by the word before the cursor and closes it on any
//! other character. Titles are sentences, so the server does the filtering on
//! the whole text after `[[`, answers incomplete lists so Helix asks again on
//! every keystroke, and asks to be triggered on spaces and punctuation.

use std::collections::BTreeMap;

use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionList, CompletionTextEdit, InsertReplaceEdit, Position, Range,
};
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use jiff::civil::Date;

use crate::ctx::Ctx;
use crate::dates::{self, Layout};
use crate::parse::Segment;
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
    complete_on(cx, rel, pos, dates::today())
}

/// `complete`, with today's date given.
pub fn complete_on(cx: &Ctx, rel: &str, pos: Position, today: Date) -> Option<CompletionList> {
    let text = cx.vault.text(rel)?;
    let (line_no, byte) = cx.byte_of(rel, pos);
    let line = *split_lines(text).get(line_no as usize)?;
    let byte = byte as usize;
    if let Some(list) = day_links(cx, rel, line_no as usize, line, byte, today) {
        return Some(list);
    }
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

    // After `#`, only the last heading (or block) is completed; the note and
    // any parent headings stay as written.
    let (start, needle, candidates) = match query.find('#') {
        Some(h) => {
            let last = query.rfind('#').unwrap() + 1;
            (open + 2 + last, &query[last..], sub_candidates(cx, rel, &query[..h], &query[h + 1..])?)
        }
        None => (open + 2, query, note_candidates(cx, embed, rel, line_no as u32, open as u32)),
    };
    // What is already after the cursor, such as the closing quote Helix
    // auto-paired in `[["quoted|"]]`.
    let tail = &line[byte..end];
    // A finished link: Helix asks again after accepting an item when the
    // name ends in punctuation; there is nothing left to offer.
    if close.is_some() && candidates.iter().any(|c| c.insert == format!("{needle}{tail}")) {
        return None;
    }
    let ranges = (cx.range(rel, Span::new(line_no, start, byte)), cx.range(rel, Span::new(line_no, start, end)));
    let mut items = rank(needle, query, candidates, ranges, closing);
    // An item ending with that text inserts only what comes before it, and
    // leaves the rest in place.
    if !tail.is_empty() {
        for item in &mut items {
            if let Some(CompletionTextEdit::InsertAndReplace(e)) = &mut item.text_edit {
                if let Some(head) = e.new_text.strip_suffix(tail) {
                    e.new_text = head.to_string();
                    e.replace = e.insert;
                }
            }
        }
    }
    Some(CompletionList { is_incomplete: true, items })
}

/// Characters of a date being typed after `@`.
fn in_day(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '/' | '-')
}

/// `@` followed by a day (`@today`, `@fri`, `@11.15`, `@2026.11.15`): items
/// that replace it with a plain link to that day's note. `@` counts only at
/// the start of a word, so email addresses are left alone.
fn day_links(cx: &Ctx, rel: &str, line_no: usize, line: &str, byte: usize, today: Date) -> Option<CompletionList> {
    let before = &line[..byte];
    let start = before.char_indices().rev().find(|&(_, c)| !in_day(c)).map_or(0, |(i, c)| i + c.len_utf8());
    let at = before[..start].strip_suffix('@')?.len();
    if !line[..at].chars().next_back().is_none_or(|c| c.is_whitespace() || matches!(c, '(' | '[' | '"')) {
        return None;
    }
    let end = byte + line[byte..].find(|c: char| !in_day(c)).unwrap_or(line.len() - byte);
    let query = &line[start..byte];
    let days = dates::days(query, today);
    if days.is_empty() {
        return None;
    }
    let vault = cx.vault;
    let layout = Layout::detect(vault.files.keys());
    let insert = cx.range(rel, Span::new(line_no, at, byte));
    let replace = cx.range(rel, Span::new(line_no, at, end));
    let items = days
        .into_iter()
        .enumerate()
        .map(|(i, (date, word))| {
            let path = layout.path(date);
            let exists = vault.files.contains_key(&path);
            // Always the full path: a bare `[[14]]` says little, and would
            // change meaning once another month has a 14th.
            let link = &path[..path.len() - 3];
            let label = date.strftime("%Y.%m.%d %a").to_string();
            let detail = match (word, exists) {
                (Some(w), true) => w.to_string(),
                (Some(w), false) => format!("{w}, not written yet"),
                (None, true) => String::new(),
                (None, false) => "not written yet".into(),
            };
            CompletionItem {
                filter_text: Some(format!("{query} {label}")),
                sort_text: Some(format!("{i:04}")),
                text_edit: Some(CompletionTextEdit::InsertAndReplace(InsertReplaceEdit {
                    new_text: format!("[[{link}]]"),
                    insert,
                    replace,
                })),
                label,
                detail: (!detail.is_empty()).then_some(detail),
                kind: Some(CompletionItemKind::FILE),
                ..Default::default()
            }
        })
        .collect();
    Some(CompletionList { is_incomplete: true, items })
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
        let link = vault.link_text(rel, current);
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
/// `sub` is everything after the first `#`; headings before the last `#`
/// narrow the list to those under them.
fn sub_candidates(cx: &Ctx, rel: &str, path: &str, sub: &str) -> Option<Vec<Candidate>> {
    let Dest::File(file) = cx.vault.resolve(rel, path) else { return Some(Vec::new()) };
    let note = cx.vault.note(&file)?;
    let blocks = sub.rsplit('#').next().is_some_and(|s| s.starts_with('^'));
    let parents: Vec<Segment> = sub
        .rsplit_once('#')
        .map(|(p, _)| p.split('#').map(|t| Segment { text: t.into(), span: Span::default(), block: false }).collect())
        .unwrap_or_default();
    let under = vault::under_path(note, &parents);
    let out = if blocks {
        note.blocks
            .iter()
            .map(|b| Candidate {
                label: format!("^{}", b.id),
                insert: format!("^{}", b.id),
                hay: format!("^{}", b.id),
                detail: None,
                kind: CompletionItemKind::REFERENCE,
                recency: 0,
            })
            .collect()
    } else {
        under
            .into_iter()
            .map(|i| &note.headings[i])
            .filter(|h| !h.text.is_empty())
            .map(|h| Candidate {
                label: h.text.clone(),
                insert: vault::heading_in_link(&h.text),
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
    let pattern = Pattern::new(needle, CaseMatching::Ignore, Normalization::Smart, AtomKind::Fuzzy);
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
            ("\"quoted text with some words\".md", ""),
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
        // An auto-paired quote after the cursor closes the title.
        let got = items("[[\"quoted\"]]", 9);
        assert_eq!((got[0].1.as_str(), got[0].3.end), ("\"quoted text with some words", Position::new(0, 9)));
        assert!(items("[[\"quoted text with some words\"]]", 30).is_empty());
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
        assert_eq!(got[0].1, "Hard puzzles]]");
        assert_eq!(got[0].2.start, Position::new(0, 16));
        let got = items("[[Combinatorics#^", 17);
        assert_eq!(got[0].1, "^p1]]");
        // `^` and `!` are text, not query syntax.
        let got = items("[[Combinatorics#^p", 18);
        assert_eq!(got[0].1, "^p1]]");
        // Under a parent: only its headings, and the parent stays.
        let got = items("[[Combinatorics#Combinatorics#", 30);
        assert_eq!(got.iter().map(|g| g.0.as_str()).collect::<Vec<_>>(), ["Hard puzzles"]);
        assert_eq!(got[0].2.start, Position::new(0, 30));
        assert!(items("[[Combinatorics#Hard puzzles]]", 28).is_empty());
        assert!(items("text [[a]] more", 15).is_empty());
        assert!(items("[[Combi|al", 10).is_empty());
    }

    #[test]
    fn day_links() {
        let today = jiff::civil::date(2026, 11, 15);
        let at = |text: &str, col: usize| {
            let v = vault(text);
            let cx = Ctx { vault: &v, enc: Encoding::Utf8 };
            let list = complete_on(&cx, "x.md", Position::new(0, col as u32), today).map(|l| l.items).unwrap_or_default();
            list.into_iter()
                .map(|i| match i.text_edit {
                    Some(CompletionTextEdit::InsertAndReplace(e)) => (i.label, e.new_text, e.insert, e.replace),
                    _ => unreachable!(),
                })
                .collect::<Vec<_>>()
        };
        let got = at("on @11.14", 9);
        assert_eq!((got[0].0.as_str(), got[0].1.as_str()), ("2026.11.14 Sat", "[[2026/11/14]]"));
        assert_eq!(got[0].2, Range::new(Position::new(0, 3), Position::new(0, 9)));
        // A day with no note yet links where it would be written.
        assert_eq!(at("@tod", 4)[0].1, "[[2026/11/15]]");
        assert_eq!(at("(@2026.10.14)", 12)[0].1, "[[2026/10/14]]");
        // Replace mode takes the rest of the date after the cursor.
        assert_eq!(at("@11.14 next", 3)[0].3.end, Position::new(0, 6));
        assert_eq!(at("@", 1).len(), 3);
        assert!(at("me@gmail.com", 12).is_empty());
        assert!(at("me@11", 5).is_empty());
        assert!(at("@someone", 8).is_empty());
    }

    #[test]
    fn unwritten_notes_are_offered() {
        let text = "[[Dynamic Call]] [[Dyna";
        let got = items(text, text.len() as u32);
        assert_eq!(got[0].0, "Dynamic Call");
    }
}
