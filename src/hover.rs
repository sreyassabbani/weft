//! Hover: what a link points at, and how much points back.

use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind, Position};

use crate::ctx::Ctx;
use crate::parse::Kind;
use crate::target::{self, On, Target};
use crate::text::split_lines;
use crate::vault::{self, Dest};

/// Lines of a note or section shown under the title.
const PREVIEW_LINES: usize = 12;

pub fn hover(cx: &Ctx, rel: &str, pos: Position) -> Option<Hover> {
    let (line, byte) = cx.byte_of(rel, pos);
    let note = cx.vault.note(rel)?;
    let (target, link) = match target::at(note, line, byte) {
        On::Link(link, _) if link.kind == Kind::Markdown => return None,
        On::Link(link, part) => (target::of_link(cx.vault, rel, link, part), Some(link)),
        On::Heading(index) => (Target::Heading { file: rel.to_string(), index }, None),
        On::Block(index) => (Target::Block { file: rel.to_string(), index }, None),
        On::Body => return None,
    };
    let refs = target::links_to(cx.vault, &target);
    let count = target::linking_notes(&refs);
    let linked = match count {
        0 => "no links".to_string(),
        1 => "linked from 1 note".to_string(),
        n => format!("linked from {n} notes"),
    };

    let value = match &target {
        Target::Note(Dest::Unwritten(_)) => format!("Not written yet · {linked}"),
        Target::Note(Dest::File(file)) => {
            let mut out = format!("**{}** · {linked}\n\n`{file}`", vault::link_name(file));
            if let Some(n) = cx.vault.note(file) {
                if !n.aliases.is_empty() {
                    out += &format!("\n\nAlso: {}", n.aliases.join(", "));
                }
                // The cursor is on the note's name, or on a heading or block
                // that is not there.
                let missing = link.is_some_and(|l| !l.subpath.is_empty() && target::at(note, line, byte).is_sub());
                if let Some(link) = link.filter(|_| missing) {
                    let what = if link.block().is_some() { "block" } else { "heading" };
                    let name = &link.subpath.last().unwrap().text;
                    out += &format!("\n\nNo {what} “{name}” in this note.");
                } else {
                    let text = cx.vault.text(file).unwrap_or("");
                    out += &preview(text, n.body_start as usize, None);
                }
            }
            out
        }
        Target::Heading { file, index } => {
            let n = cx.vault.note(file)?;
            let h = &n.headings[*index];
            let mut out = format!("**{}** · {linked}", h.text);
            if file != rel {
                out += &format!("\n\n`{file}`");
            }
            if link.is_some() {
                let end = n.headings.get(index + 1).map(|h| h.span.line as usize);
                out += &preview(cx.vault.text(file).unwrap_or(""), h.span.line as usize + 1, end);
            }
            out
        }
        Target::Block { file, index } => {
            let n = cx.vault.note(file)?;
            let b = &n.blocks[*index];
            let mut out = format!("**^{}** · {linked}", b.id);
            if link.is_some() {
                let text = split_lines(cx.vault.text(file).unwrap_or(""));
                let line = text.get(b.span.line as usize).copied().unwrap_or("");
                out += &format!("\n\n---\n\n{}", line.trim());
            }
            out
        }
    };
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent { kind: MarkupKind::Markdown, value }),
        range: None,
    })
}

/// The first non-empty lines from `start`, up to `end`.
fn preview(text: &str, start: usize, end: Option<usize>) -> String {
    let lines = split_lines(text);
    let end = end.unwrap_or(lines.len()).min(lines.len());
    let body: Vec<&str> = lines[start.min(end)..end]
        .iter()
        .copied()
        .skip_while(|l| l.trim().is_empty())
        .take(PREVIEW_LINES)
        .collect();
    let body = body.join("\n");
    let body = body.trim_end();
    if body.is_empty() { String::new() } else { format!("\n\n---\n\n{body}") }
}
