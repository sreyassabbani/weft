//! Go to definition and find references.

use lsp_types::{Location, Position, Range, Url};

use crate::ctx::Ctx;
use crate::parse::{self, Kind, Link};
use crate::target::{self, On, Target};
use crate::text::Span;
use crate::vault::{self, Dest};

/// Only links have a definition: the note, heading or block they name, or
/// where a note not written yet would be created.
pub fn definition(cx: &Ctx, rel: &str, pos: Position) -> Option<Location> {
    let (line, byte) = cx.byte_of(rel, pos);
    let note = cx.vault.note(rel)?;
    let On::Link(link, part) = target::at(note, line, byte) else { return None };
    if link.kind == Kind::Markdown {
        return markdown_definition(cx, rel, link);
    }
    // A link to a heading goes to the heading wherever the cursor is in it.
    let part = if link.subpath.is_empty() { part } else { target::Part::Sub };
    Some(match target::of_link(cx.vault, rel, link, part) {
        Target::Note(Dest::File(file)) => cx.start_of(&file),
        Target::Note(Dest::Unwritten(_)) => {
            let path = cx.vault.create_path(rel, &link.path)?;
            cx.start_of(&path)
        }
        t => {
            let (file, span) = target::declaration(cx.vault, &t)?;
            cx.location(&file, span)
        }
    })
}

/// `[text](relative/path.md#heading)`: the file, if it exists.
fn markdown_definition(cx: &Ctx, rel: &str, link: &Link) -> Option<Location> {
    let base = cx.vault.path(rel);
    let path = if link.path.is_empty() {
        base.clone()
    } else if let Some(from_root) = link.path.strip_prefix('/') {
        cx.vault.root.join(from_root)
    } else {
        base.parent()?.join(&link.path)
    };
    if !path.is_file() {
        return None;
    }
    let path = path.canonicalize().unwrap_or(path);
    let uri = Url::from_file_path(&path).ok()?;
    let start = Range::default();
    if link.subpath.is_empty() {
        return Some(Location::new(uri, start));
    }
    // Headings are only looked up in Markdown files.
    let text = match cx.vault.rel(&path) {
        Some(r) if cx.vault.text(&r).is_some() => cx.vault.text(&r)?.to_string(),
        _ if vault::is_note(&path.to_string_lossy()) => std::fs::read_to_string(&path).ok()?,
        _ => return Some(Location::new(uri, start)),
    };
    let note = parse::parse(&text);
    let range = match vault::find_heading(&note, link) {
        Some(i) => heading_range(cx, &text, note.headings[i].span),
        None => start,
    };
    Some(Location::new(uri, range))
}

fn heading_range(cx: &Ctx, text: &str, span: Span) -> Range {
    crate::text::Lines::new(text).range(span, cx.enc)
}

pub fn references(cx: &Ctx, rel: &str, pos: Position, declaration: bool) -> Vec<Location> {
    let (line, byte) = cx.byte_of(rel, pos);
    let Some(target) = target::under(cx.vault, rel, line, byte) else { return Vec::new() };
    let mut out = Vec::new();
    if declaration {
        if let Some((file, span)) = target::declaration(cx.vault, &target) {
            out.push(cx.location(&file, span));
        }
    }
    for r in target::links_to(cx.vault, &target) {
        out.push(cx.location(r.file, r.link.span));
    }
    out
}
