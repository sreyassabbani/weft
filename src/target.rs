//! What the cursor is on, and which links point at it.

use crate::parse::{Kind, Link, Note};
use crate::text::Span;
use crate::vault::{self, Dest, Vault};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Note(Dest),
    Heading { file: String, index: usize },
    Block { file: String, index: usize },
}

/// The part of a note under the cursor.
pub enum On<'a> {
    Link(&'a Link, Part),
    Heading(usize),
    Block(usize),
    Body,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// The note or file part, or the brackets around it.
    Path,
    /// After the path, in a link with a `#heading` or `#^block` part.
    Sub,
}

impl On<'_> {
    pub fn is_sub(&self) -> bool {
        matches!(self, On::Link(_, Part::Sub))
    }
}

pub fn at(note: &Note, line: u32, byte: u32) -> On<'_> {
    // Between two adjacent links, `[[a]][[b]]`, the cursor is on the second.
    let starts = |l: &&Link| l.span.line == line && l.span.start <= byte && byte < l.span.end;
    let link = note.links.iter().find(starts).or_else(|| note.links.iter().find(|l| l.span.contains(line, byte)));
    if let Some(link) = link {
        let sub = !link.subpath.is_empty() && byte > link.path_span.end;
        return On::Link(link, if sub { Part::Sub } else { Part::Path });
    }
    if let Some(i) = note.headings.iter().position(|h| h.span.line == line) {
        return On::Heading(i);
    }
    if let Some(i) = note.blocks.iter().position(|b| b.span.contains(line, byte + 1)) {
        return On::Block(i);
    }
    On::Body
}

/// What a wikilink in `from` points at. With `Part::Path`, the note even if
/// the link names a heading in it.
pub fn of_link(vault: &Vault, from: &str, link: &Link, part: Part) -> Target {
    let dest = vault.resolve(from, &link.path);
    if part == Part::Sub {
        if let Some(t) = sub_target(vault, &dest, link) {
            return t;
        }
    }
    Target::Note(dest)
}

fn sub_target(vault: &Vault, dest: &Dest, link: &Link) -> Option<Target> {
    let Dest::File(file) = dest else { return None };
    let note = vault.note(file)?;
    if let Some(block) = link.block() {
        let index = vault::find_block(note, &block.text)?;
        return Some(Target::Block { file: file.clone(), index });
    }
    let index = vault::find_heading(note, link)?;
    Some(Target::Heading { file: file.clone(), index })
}

/// The target under the cursor in `rel`. Outside links, headings and block
/// ids it is the note itself.
pub fn under(vault: &Vault, rel: &str, line: u32, byte: u32) -> Option<Target> {
    let note = vault.note(rel)?;
    Some(match at(note, line, byte) {
        On::Link(link, _) if link.kind == Kind::Markdown => return None,
        On::Link(link, part) => of_link(vault, rel, link, part),
        On::Heading(index) => Target::Heading { file: rel.to_string(), index },
        On::Block(index) => Target::Block { file: rel.to_string(), index },
        On::Body => Target::Note(Dest::File(rel.to_string())),
    })
}

/// A wikilink or day pointing at a target.
pub struct Ref<'a> {
    pub file: &'a String,
    pub link: &'a Link,
}

/// Every wikilink or day pointing at `target`. A link to a heading or block counts
/// toward its note too.
pub fn links_to<'a>(vault: &'a Vault, target: &Target) -> Vec<Ref<'a>> {
    let (dest, file) = match target {
        Target::Note(d) => (d.clone(), None),
        Target::Heading { file, .. } | Target::Block { file, .. } => {
            (Dest::File(file.clone()), vault.note(file))
        }
    };
    let mut out = Vec::new();
    for (from, note) in vault.notes() {
        for link in note.links.iter().filter(|l| l.kind != Kind::Markdown) {
            if vault.resolve(from, &link.path) != dest {
                continue;
            }
            let hit = match (target, file) {
                (Target::Note(_), _) => true,
                (Target::Heading { index, .. }, Some(n)) => {
                    link.block().is_none() && vault::find_heading(n, link) == Some(*index)
                }
                (Target::Block { index, .. }, Some(n)) => {
                    link.block().is_some_and(|b| vault::find_block(n, &b.text) == Some(*index))
                }
                _ => false,
            };
            if hit {
                out.push(Ref { file: from, link });
            }
        }
    }
    out
}

/// The span of the declaration of a heading or block.
pub fn declaration(vault: &Vault, target: &Target) -> Option<(String, Span)> {
    match target {
        Target::Heading { file, index } => Some((file.clone(), vault.note(file)?.headings[*index].span)),
        Target::Block { file, index } => Some((file.clone(), vault.note(file)?.blocks[*index].span)),
        Target::Note(_) => None,
    }
}

/// How many distinct notes link to `target`.
pub fn linking_notes(refs: &[Ref]) -> usize {
    refs.iter().map(|r| r.file).collect::<std::collections::BTreeSet<_>>().len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault() -> Vault {
        Vault::from_files(&[
            ("Array.md", "# Array\n## C++\n### Dynamic Allocation\nend ^blk\n"),
            ("Sorting.md", "To sort [[Array#C++#Dynamic Allocation|vectors]] and [[array]].\n[[Array#^blk]] [[Dynamic Call]]"),
            ("Other.md", "[[Dynamic Call]] [[dynamic call|lb]] [[Array#C++]]"),
        ])
    }

    #[test]
    fn cursor_parts() {
        let v = vault();
        let note = v.note("Sorting.md").unwrap();
        assert!(matches!(at(note, 0, 10), On::Link(_, Part::Path)));
        assert!(matches!(at(note, 0, 17), On::Link(_, Part::Sub)));
        assert!(matches!(at(note, 0, 42), On::Link(_, Part::Sub)));
        assert!(matches!(at(note, 0, 2), On::Body));
        let adjacent = Vault::from_files(&[("a.md", "[[a]][[b]]")]);
        let On::Link(l, _) = at(adjacent.note("a.md").unwrap(), 0, 5) else { panic!() };
        assert_eq!(l.path, "b");
        let t = under(&v, "Sorting.md", 0, 25).unwrap();
        assert_eq!(t, Target::Heading { file: "Array.md".into(), index: 2 });
        assert_eq!(under(&v, "Sorting.md", 0, 10).unwrap(), Target::Note(Dest::File("Array.md".into())));
        assert_eq!(under(&v, "Array.md", 3, 6).unwrap(), Target::Block { file: "Array.md".into(), index: 0 });
    }

    #[test]
    fn weft_references() {
        let v = Vault::weft_from_files(&[
            ("2026/11/14.md", ""),
            ("a.md", "[2026/11/14] and @2026.11.14, [[2026/11/14]]"),
        ]);
        let refs = links_to(&v, &Target::Note(Dest::File("2026/11/14.md".into())));
        assert_eq!(refs.len(), 3);
        assert_eq!(under(&v, "a.md", 0, 20).unwrap(), Target::Note(Dest::File("2026/11/14.md".into())));
    }

    #[test]
    fn references() {
        let v = vault();
        let note = Target::Note(Dest::File("Array.md".into()));
        assert_eq!(links_to(&v, &note).len(), 4);
        let heading = Target::Heading { file: "Array.md".into(), index: 1 };
        assert_eq!(links_to(&v, &heading).len(), 1);
        let block = Target::Block { file: "Array.md".into(), index: 0 };
        assert_eq!(links_to(&v, &block).len(), 1);
        let unwritten = Target::Note(Dest::Unwritten("dynamic call".into()));
        let refs = links_to(&v, &unwritten);
        assert_eq!((refs.len(), linking_notes(&refs)), (3, 2));
    }
}
