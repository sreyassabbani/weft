//! Diagnostics, only for what is broken regardless of intent: a heading or
//! block missing from a note that exists, and a missing embedded file.
//! A link to a note not written yet is not one of them.

use lsp_types::{Diagnostic, DiagnosticSeverity};

use crate::ctx::Ctx;
use crate::parse::Kind;
use crate::vault::{self, Dest};

pub fn diagnose(cx: &Ctx, rel: &str) -> Vec<Diagnostic> {
    let Some(note) = cx.vault.note(rel) else { return Vec::new() };
    let mut out = Vec::new();
    for link in note.links.iter().filter(|l| l.kind == Kind::Wiki) {
        let dest = cx.vault.resolve(rel, &link.path);
        let message = match &dest {
            Dest::Unwritten(_) if link.embed && vault::is_attachment(&link.path) => {
                if cx.vault.on_disk(rel, &link.path) {
                    continue;
                }
                format!("No file “{}” in the vault", link.path)
            }
            Dest::File(file) if !link.subpath.is_empty() => {
                let Some(target) = cx.vault.note(file) else { continue };
                let name = vault::link_name(file);
                if let Some(block) = link.block() {
                    if vault::find_block(target, &block.text).is_some() {
                        continue;
                    }
                    format!("No block “^{}” in “{name}”", block.text)
                } else {
                    if vault::find_heading(target, link).is_some() {
                        continue;
                    }
                    format!("No heading “{}” in “{name}”", link.subpath.last().unwrap().text)
                }
            }
            _ => continue,
        };
        let span = match link.subpath.first() {
            Some(first) => crate::text::Span { end: link.subpath.last().unwrap().span.end, ..first.span },
            None => link.path_span,
        };
        out.push(Diagnostic {
            range: cx.range(rel, span),
            severity: Some(DiagnosticSeverity::WARNING),
            source: Some("weft".into()),
            message,
            ..Default::default()
        });
    }
    out
}
