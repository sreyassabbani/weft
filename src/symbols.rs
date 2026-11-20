//! Document symbols (the outline) and workspace symbols (notes and headings).

use lsp_types::{DocumentSymbol, Location, SymbolInformation, SymbolKind};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::ctx::Ctx;
use crate::text::Span;
use crate::vault;

const WORKSPACE_LIMIT: usize = 100;

#[allow(deprecated)]
pub fn document(cx: &Ctx, rel: &str) -> Vec<DocumentSymbol> {
    let Some(note) = cx.vault.note(rel) else { return Vec::new() };
    let text = cx.vault.text(rel).unwrap_or("");
    let last_line = text.lines().count().saturating_sub(1) as u32;
    // Each heading spans to the next heading at its level or above.
    let mut flat: Vec<(u8, DocumentSymbol)> = Vec::new();
    for (i, h) in note.headings.iter().enumerate() {
        let end = vault::section(note, i).end;
        let end_line = note.headings.get(end).map_or(last_line, |h| h.span.line.saturating_sub(1));
        let line_len = text.lines().nth(end_line as usize).map_or(0, str::len) as u32;
        let full = Span { line: h.span.line, start: 0, end: h.span.end };
        let mut range = cx.range(rel, full);
        range.end = cx.range(rel, Span { line: end_line, start: line_len, end: line_len }).end;
        flat.push((
            h.level,
            DocumentSymbol {
                name: if h.text.is_empty() { "(empty)".into() } else { h.text.clone() },
                detail: None,
                kind: SymbolKind::STRING,
                tags: None,
                deprecated: None,
                range,
                selection_range: cx.range(rel, h.span),
                children: Some(Vec::new()),
            },
        ));
    }
    nest(flat)
}

/// Builds the heading tree from headings in order.
fn nest(flat: Vec<(u8, DocumentSymbol)>) -> Vec<DocumentSymbol> {
    let mut roots: Vec<DocumentSymbol> = Vec::new();
    let mut stack: Vec<(u8, DocumentSymbol)> = Vec::new();
    let pop = |stack: &mut Vec<(u8, DocumentSymbol)>, roots: &mut Vec<DocumentSymbol>| {
        let (_, sym) = stack.pop().unwrap();
        match stack.last_mut() {
            Some((_, parent)) => parent.children.get_or_insert_with(Vec::new).push(sym),
            None => roots.push(sym),
        }
    };
    for (level, sym) in flat {
        while stack.last().is_some_and(|(l, _)| *l >= level) {
            pop(&mut stack, &mut roots);
        }
        stack.push((level, sym));
    }
    while !stack.is_empty() {
        pop(&mut stack, &mut roots);
    }
    roots
}

/// Notes and headings matching `query`, best first. Flat, as Helix wants.
#[allow(deprecated)]
pub fn workspace(cx: &Ctx, query: &str) -> Vec<SymbolInformation> {
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, SymbolInformation)> = Vec::new();
    for (rel, note) in cx.vault.notes() {
        let name = vault::link_name(rel);
        let mut push = |hay: &str, sym: SymbolInformation| {
            let score = if query.is_empty() { Some(0) } else { pattern.score(Utf32Str::new(hay, &mut buf), &mut matcher) };
            if let Some(score) = score {
                scored.push((score, sym));
            }
        };
        let info = |name: String, kind, location: Location, container: Option<String>| SymbolInformation {
            name,
            kind,
            tags: None,
            deprecated: None,
            location,
            container_name: container,
        };
        push(rel, info(name.to_string(), SymbolKind::FILE, cx.start_of(rel), None));
        for h in &note.headings {
            let hay = format!("{name} {}", h.text);
            push(&hay, info(h.text.clone(), SymbolKind::STRING, cx.location(rel, h.span), Some(name.to_string())));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.len().cmp(&b.1.name.len())));
    scored.into_iter().take(WORKSPACE_LIMIT).map(|(_, s)| s).collect()
}
