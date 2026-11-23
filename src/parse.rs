//! A single pass over the lines of a note: frontmatter aliases, headings,
//! block ids and links.

use jiff::civil::Date;

use crate::dates::Layout;
use crate::text::{Span, split_lines};

/// The link forms a vault's notes are written in.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum Syntax {
    /// Obsidian's: `[[note]]`, and `[text](path)`.
    #[default]
    Obsidian,
    /// weft's, in a vault marked by a `.weft` file: Obsidian's, plus `[note]`
    /// and `@2026.11.22` for a day's note, which lives where the vault's
    /// daily notes do.
    Weft(Layout),
}

#[derive(Debug, Default, Clone)]
pub struct Note {
    pub links: Vec<Link>,
    pub headings: Vec<Heading>,
    pub blocks: Vec<Block>,
    pub aliases: Vec<String>,
    /// Number of lines taken by frontmatter.
    pub body_start: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `[[path#sub|alias]]`
    Wiki,
    /// `[text](path#heading)`, kept for go-to-definition only.
    Markdown,
    /// `@2026.11.22`: the day's note, by its path.
    Day,
}

#[derive(Debug, Clone)]
pub struct Link {
    pub kind: Kind,
    pub embed: bool,
    /// The whole link, including brackets and a leading `!`.
    pub span: Span,
    /// The note or file part, trimmed; empty for `[[#Heading]]`.
    pub path: String,
    pub path_span: Span,
    /// Headings after `#`, or a single block id after `#^`.
    pub subpath: Vec<Segment>,
}

#[derive(Debug, Clone)]
pub struct Segment {
    /// Without the `#`, and for a block without the `^`.
    pub text: String,
    pub span: Span,
    pub block: bool,
}

impl Link {
    pub fn block(&self) -> Option<&Segment> {
        self.subpath.last().filter(|s| s.block)
    }

    pub fn headings(&self) -> &[Segment] {
        if self.block().is_some() { &[] } else { &self.subpath }
    }
}

#[derive(Debug, Clone)]
pub struct Heading {
    pub level: u8,
    pub text: String,
    /// The heading text, without the `#` marks.
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Block {
    pub id: String,
    /// The id, without the `^`.
    pub span: Span,
}

/// A note in Obsidian's syntax.
pub fn parse(text: &str) -> Note {
    parse_with(text, &Syntax::Obsidian)
}

pub fn parse_with(text: &str, syntax: &Syntax) -> Note {
    let lines = split_lines(text);
    let mut note = Note::default();
    let mut i = 0;

    if lines.first().map(|l| l.trim_end()) == Some("---") {
        if let Some(end) = lines[1..].iter().position(|l| matches!(l.trim_end(), "---" | "...")) {
            note.aliases = aliases(&lines[1..=end]);
            i = end + 2;
            note.body_start = i as u32;
        }
    }

    let mut fence: Option<(char, usize)> = None;
    let mut math = false;
    for (n, line) in lines.iter().enumerate().skip(i) {
        let trimmed = line.trim_start();
        if let Some((ch, len)) = fence {
            let run = trimmed.chars().take_while(|&c| c == ch).count();
            if run >= len && trimmed[run..].trim().is_empty() {
                fence = None;
            }
            continue;
        }
        let run = trimmed.chars().take_while(|&c| c == '`' || c == '~').count();
        if run >= 3 && trimmed[..run].chars().all(|c| c == trimmed.as_bytes()[0] as char) {
            fence = Some((trimmed.as_bytes()[0] as char, run));
            continue;
        }
        if math {
            if line.matches("$$").count() % 2 == 1 {
                math = false;
            }
            continue;
        }
        if trimmed.starts_with("$$") && line.matches("$$").count() % 2 == 1 {
            math = true;
            continue;
        }

        let mut code = code_spans(line);
        code.extend(math_spans(line, &code));
        if let Some(h) = heading(n, line) {
            note.headings.push(h);
        }
        if let Some(b) = block(n, line) {
            note.blocks.push(b);
        }
        let first = note.links.len();
        wikilinks(n, line, &code, &mut note.links);
        markdown_links(n, line, &code, &mut note.links);
        if let Syntax::Weft(layout) = syntax {
            let taken: Vec<(usize, usize)> =
                note.links[first..].iter().map(|l| (l.span.start as usize, l.span.end as usize)).collect();
            brackets(n, line, &code, &taken, &mut note.links);
            days(n, line, &code, layout, &mut note.links);
        }
    }
    note.links.sort_by_key(|l| (l.span.line, l.span.start));
    note
}

/// `aliases:` (or `alias:`) as a scalar, an inline list or a block list.
fn aliases(lines: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_list = false;
    for line in lines {
        if in_list {
            if let Some(item) = line.trim_start().strip_prefix("- ") {
                push_alias(&mut out, item);
                continue;
            }
            if line.starts_with(' ') || line.trim().is_empty() {
                continue;
            }
            in_list = false;
        }
        let Some((key, value)) = line.split_once(':') else { continue };
        if !matches!(key, "aliases" | "alias") {
            continue;
        }
        let value = value.trim();
        if value.is_empty() {
            in_list = true;
        } else if let Some(inner) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
            inner.split(',').for_each(|a| push_alias(&mut out, a));
        } else {
            push_alias(&mut out, value);
        }
    }
    out
}

fn push_alias(out: &mut Vec<String>, raw: &str) {
    let a = raw.trim();
    let a = a
        .strip_prefix('"')
        .and_then(|a| a.strip_suffix('"'))
        .or_else(|| a.strip_prefix('\'').and_then(|a| a.strip_suffix('\'')))
        .unwrap_or(a)
        .trim();
    if !a.is_empty() {
        out.push(a.to_string());
    }
}

fn heading(n: usize, line: &str) -> Option<Heading> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let level = rest.bytes().take_while(|&b| b == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let after = &rest[level..];
    if !(after.is_empty() || after.starts_with(' ') || after.starts_with('\t')) {
        return None;
    }
    let mut text = after.trim();
    // An optional closing sequence: `## Title ##`.
    let closed = text.trim_end_matches('#');
    if closed.len() < text.len() && (closed.is_empty() || closed.ends_with(' ')) {
        text = closed.trim_end();
    }
    let start = if text.is_empty() {
        line.len()
    } else {
        indent + level + after.find(text).unwrap_or(0)
    };
    Some(Heading {
        level: level as u8,
        text: text.to_string(),
        span: Span::new(n, start, start + text.len()),
    })
}

/// A `^id` at the end of a line, after whitespace or alone on the line.
fn block(n: usize, line: &str) -> Option<Block> {
    let body = line.trim_end();
    let caret = body.rfind('^')?;
    let id = &body[caret + 1..];
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return None;
    }
    if caret > 0 && !body[..caret].ends_with([' ', '\t']) {
        return None;
    }
    Some(Block { id: id.to_string(), span: Span::new(n, caret + 1, body.len()) })
}

/// Byte ranges covered by inline code.
fn code_spans(line: &str) -> Vec<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'`' {
            i += 1;
            continue;
        }
        let run = bytes[i..].iter().take_while(|&&b| b == b'`').count();
        let mut j = i + run;
        let mut closed = None;
        while j < bytes.len() {
            if bytes[j] == b'`' {
                let r = bytes[j..].iter().take_while(|&&b| b == b'`').count();
                if r == run {
                    closed = Some(j + r);
                    break;
                }
                j += r;
            } else {
                j += 1;
            }
        }
        match closed {
            Some(end) => {
                out.push((i, end));
                i = end;
            }
            None => i += run,
        }
    }
    out
}

/// `$$…$$` within one line, outside inline code.
fn math_spans(line: &str, code: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let marks: Vec<usize> = line.match_indices("$$").map(|(i, _)| i).filter(|&i| !in_code(code, i)).collect();
    marks.chunks_exact(2).map(|p| (p[0], p[1] + 2)).collect()
}

fn in_code(code: &[(usize, usize)], i: usize) -> bool {
    code.iter().any(|&(a, b)| a <= i && i < b)
}

fn wikilinks(n: usize, line: &str, code: &[(usize, usize)], out: &mut Vec<Link>) {
    let mut from = 0;
    while let Some(found) = line[from..].find("[[") {
        let open = from + found;
        let inner = open + 2;
        let Some(close) = line[inner..].find("]]").map(|i| inner + i) else { return };
        // `[[a [[b]]` links `b`.
        if let Some(again) = line[inner..close].rfind("[[") {
            from = inner + again;
            continue;
        }
        from = close + 2;
        if in_code(code, open) {
            continue;
        }
        let embed = open > 0 && line.as_bytes()[open - 1] == b'!';
        let start = if embed { open - 1 } else { open };
        out.push(wikilink(n, line, start, inner, close, close + 2, embed));
    }
}

/// `[note]`, in weft's syntax: a wikilink with single brackets. Left alone:
/// checkboxes (`[ ]`, `[x]`), footnotes (`[^1]`), Markdown's own links and
/// reference definitions, escaped brackets, and text with no letter or
/// digit in it, such as `[...]`.
fn brackets(n: usize, line: &str, code: &[(usize, usize)], taken: &[(usize, usize)], out: &mut Vec<Link>) {
    let bytes = line.as_bytes();
    let mut from = 0;
    while let Some(found) = line[from..].find('[') {
        let open = from + found;
        from = open + 1;
        let Some(close) = line[open + 1..].find(['[', ']']).map(|i| open + 1 + i) else { return };
        if bytes[close] == b'[' {
            continue;
        }
        let before = open.checked_sub(1).map(|i| bytes[i]);
        let after = bytes.get(close + 1).copied();
        let content = &line[open + 1..close];
        let skip = in_code(code, open)
            || taken.iter().any(|&(a, b)| a <= open && open < b)
            || matches!(before, Some(b'\\' | b'[' | b']'))
            || matches!(after, Some(b'(' | b'['))
            || after == Some(b':') && line[..open].trim().is_empty()
            || matches!(content, "x" | "X")
            || content.starts_with('^')
            || !content.chars().any(char::is_alphanumeric);
        if skip {
            continue;
        }
        let embed = before == Some(b'!');
        out.push(wikilink(n, line, if embed { open - 1 } else { open }, open + 1, close, close + 1, embed));
        from = close + 1;
    }
}

/// `@2026.11.22` at the start of a word, in weft's syntax.
fn days(n: usize, line: &str, code: &[(usize, usize)], layout: &Layout, out: &mut Vec<Link>) {
    for (at, _) in line.match_indices('@') {
        if !line[..at].chars().next_back().is_none_or(|c| c.is_whitespace() || matches!(c, '(' | '[' | '"')) {
            continue;
        }
        let Some(date) = line.get(at + 1..at + 11).and_then(day) else { continue };
        // Not part of something longer: `@2026.11.22.` ends a sentence, but
        // `@2026.11.223` is not a day.
        let rest = &line[at + 11..];
        let mut next = rest.chars();
        let longer = match next.next() {
            Some(c) if c.is_alphanumeric() || c == '_' => true,
            Some('.' | '/' | '-') => next.next().is_some_and(|c| c.is_ascii_digit()),
            _ => false,
        };
        if longer || in_code(code, at) {
            continue;
        }
        let path = layout.path(date);
        out.push(Link {
            kind: Kind::Day,
            embed: false,
            span: Span::new(n, at, at + 11),
            path: path[..path.len() - 3].to_string(),
            path_span: Span::new(n, at + 1, at + 11),
            subpath: Vec::new(),
        });
    }
}

/// `2026.11.22`, exactly.
pub fn day(s: &str) -> Option<Date> {
    let b = s.as_bytes();
    let shape = b.len() == 10
        && b.iter().enumerate().all(|(i, &c)| if i == 4 || i == 7 { c == b'.' } else { c.is_ascii_digit() });
    if !shape {
        return None;
    }
    Date::new(s[..4].parse().ok()?, s[5..7].parse().ok()?, s[8..].parse().ok()?).ok()
}

/// How `syntax` writes a link to `rel` whose text is `text`: a day's note as
/// `@2026.11.22` and others in single brackets in weft's syntax, where that
/// cannot be misread; otherwise in double brackets.
pub fn write_link(syntax: &Syntax, text: &str, rel: &str) -> String {
    if let Syntax::Weft(layout) = syntax {
        if let Some(date) = layout.date(rel) {
            return format!("@{}", date.strftime("%Y.%m.%d"));
        }
        let plain = !matches!(text, "x" | "X")
            && !text.starts_with('^')
            && !text.contains(['[', ']'])
            && text.chars().any(char::is_alphanumeric);
        if plain {
            return format!("[{text}]");
        }
    }
    format!("[[{text}]]")
}

fn wikilink(n: usize, line: &str, start: usize, inner: usize, close: usize, end: usize, embed: bool) -> Link {
    let content = &line[inner..close];
    // The alias starts at the first `|`; in tables it is written `\|`.
    let target_end = match content.find('|') {
        Some(bar) => inner + if content[..bar].ends_with('\\') { bar - 1 } else { bar },
        None => close,
    };
    let target = &line[inner..target_end];
    let hash = target.find('#').map_or(target_end, |h| inner + h);
    let (path, path_span) = trimmed(n, line, inner, hash);

    let mut subpath = Vec::new();
    let mut at = hash;
    while at < target_end {
        let seg_start = at + 1;
        let seg_end = line[seg_start..target_end].find('#').map_or(target_end, |i| seg_start + i);
        let (text, span) = trimmed(n, line, seg_start, seg_end);
        if let Some(id) = text.strip_prefix('^') {
            let span = Span { start: span.start + 1, ..span };
            subpath = vec![Segment { text: id.to_string(), span, block: true }];
            break;
        }
        if !text.is_empty() {
            subpath.push(Segment { text, span, block: false });
        }
        at = seg_end;
    }

    Link {
        kind: Kind::Wiki,
        embed,
        span: Span::new(n, start, end),
        path,
        path_span,
        subpath,
    }
}

fn trimmed(n: usize, line: &str, a: usize, b: usize) -> (String, Span) {
    let raw = &line[a..b];
    let lead = raw.len() - raw.trim_start().len();
    let text = raw.trim();
    (text.to_string(), Span::new(n, a + lead, a + lead + text.len()))
}

/// `[text](dest)` and `![alt](dest)` whose destination is a relative path.
fn markdown_links(n: usize, line: &str, code: &[(usize, usize)], out: &mut Vec<Link>) {
    let bytes = line.as_bytes();
    let mut from = 0;
    while let Some(found) = line[from..].find("](") {
        let mid = from + found;
        from = mid + 2;
        if in_code(code, mid) {
            continue;
        }
        // Find the `[` that opens the text, allowing nested brackets.
        let mut depth = 0;
        let mut open = None;
        for i in (0..mid).rev() {
            match bytes[i] {
                b']' => depth += 1,
                b'[' if depth == 0 => {
                    open = Some(i);
                    break;
                }
                b'[' => depth -= 1,
                _ => {}
            }
        }
        let Some(open) = open else { continue };
        if open > 0 && bytes[open - 1] == b'[' || bytes.get(open + 1) == Some(&b'[') {
            continue; // part of a wikilink
        }
        let dest_start = mid + 2;
        let (dest_a, dest_b, close) = if bytes.get(dest_start) == Some(&b'<') {
            let Some(end) = line[dest_start..].find('>') else { continue };
            let end = dest_start + end;
            let Some(close) = line[end..].find(')') else { continue };
            (dest_start + 1, end, end + close)
        } else {
            let mut depth = 0;
            let mut end = None;
            for (i, c) in line[dest_start..].char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' if depth == 0 => {
                        end = Some(dest_start + i);
                        break;
                    }
                    ')' => depth -= 1,
                    _ => {}
                }
            }
            let Some(close) = end else { continue };
            let dest_end = line[dest_start..close].find(' ').map_or(close, |i| dest_start + i);
            (dest_start, dest_end, close)
        };
        from = close + 1;
        let dest = &line[dest_a..dest_b];
        if dest.is_empty() || has_scheme(dest) || dest.starts_with("//") {
            continue;
        }
        let (path_raw, frag) = match dest.find('#') {
            Some(h) => (&dest[..h], Some((dest_a + h + 1, &dest[h + 1..]))),
            None => (dest, None),
        };
        let subpath = frag
            .filter(|(_, f)| !f.is_empty())
            .map(|(at, f)| {
                vec![Segment {
                    text: decode(f),
                    span: Span::new(n, at, at + f.len()),
                    block: false,
                }]
            })
            .unwrap_or_default();
        let embed = open > 0 && bytes[open - 1] == b'!';
        out.push(Link {
            kind: Kind::Markdown,
            embed,
            span: Span::new(n, if embed { open - 1 } else { open }, close + 1),
            path: decode(path_raw),
            path_span: Span::new(n, dest_a, dest_a + path_raw.len()),
            subpath,
        });
    }
}

fn has_scheme(dest: &str) -> bool {
    match dest.find(':') {
        Some(i) => {
            let s = &dest[..i];
            !s.is_empty()
                && s.as_bytes()[0].is_ascii_alphabetic()
                && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"+.-".contains(&b))
        }
        None => false,
    }
}

fn decode(s: &str) -> String {
    percent_encoding::percent_decode_str(s).decode_utf8_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links(text: &str) -> Vec<Link> {
        parse(text).links
    }

    #[test]
    fn wikilink_parts() {
        let l = &links("see [[Array#C++#Dynamic Allocation|vectors]] here")[0];
        assert_eq!(l.path, "Array");
        let subs: Vec<_> = l.subpath.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(subs, ["C++", "Dynamic Allocation"]);
        assert_eq!((l.span.start, l.span.end), (4, 44));
        assert_eq!((l.path_span.start, l.path_span.end), (6, 11));
    }

    #[test]
    fn titles_with_punctuation() {
        let l = &links(r#"[["quoted text with some words"]] and [[Aren't we just going round?]]"#);
        assert_eq!(l[0].path, "\"quoted text with some words\"");
        assert_eq!(l[1].path, "Aren't we just going round?");
    }

    #[test]
    fn embeds_sizes_and_folders() {
        let l = &links("![[Screenshot 2026-01-02 at 3.04.05 PM.png|313]] [[2026/11/14]]");
        assert!(l[0].embed);
        assert_eq!(l[0].path, "Screenshot 2026-01-02 at 3.04.05 PM.png");
        assert!(!l[1].embed);
        assert_eq!(l[1].path, "2026/11/14");
    }

    #[test]
    fn same_note_and_blocks() {
        let l = &links("[[#Heading]] [[Note#^abc-1]] [[#^x]]");
        assert_eq!(l[0].path, "");
        assert_eq!(l[0].subpath[0].text, "Heading");
        assert_eq!(l[1].block().unwrap().text, "abc-1");
        assert!(l[1].headings().is_empty());
        assert_eq!(l[2].path, "");
        assert_eq!(l[2].block().unwrap().text, "x");
    }

    #[test]
    fn table_pipe() {
        let l = &links(r"| [[Note\|alias]] |");
        assert_eq!(l[0].path, "Note");
        assert_eq!((l[0].path_span.start, l[0].path_span.end), (4, 8));
    }

    #[test]
    fn code_and_math_are_not_links() {
        let text = "`[[a]]` [[b]]\n```\n[[c]]\n# not a heading\n```\n$$\n[[d]]\n$$\n[[e]] $$ [[f]] $$ `$$` [[g]] `$$`";
        let note = parse(text);
        let paths: Vec<_> = note.links.iter().map(|l| l.path.as_str()).collect();
        assert_eq!(paths, ["b", "e", "g"]);
        assert!(note.headings.is_empty());
    }

    #[test]
    fn footnotes_are_not_blocks() {
        let note = parse("text[^1]\n[^1]: a footnote\nparagraph ^real-id\n^alone");
        let ids: Vec<_> = note.blocks.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(ids, ["real-id", "alone"]);
        assert_eq!((note.blocks[0].span.start, note.blocks[0].span.end), (11, 18));
    }

    #[test]
    fn headings() {
        let note = parse("# One\n## Two ##\n#hashtag\n####### seven\n   ### Three");
        let h: Vec<_> = note.headings.iter().map(|h| (h.level, h.text.as_str())).collect();
        assert_eq!(h, [(1, "One"), (2, "Two"), (3, "Three")]);
        assert_eq!((note.headings[1].span.start, note.headings[1].span.end), (3, 6));
    }

    #[test]
    fn frontmatter() {
        let text = "---\naliases:\n  - Neighbour Graphs\n  - \"Other\"\ntags: [x]\n---\n# H\n[[a]]";
        let note = parse(text);
        assert_eq!(note.aliases, ["Neighbour Graphs", "Other"]);
        assert_eq!(note.body_start, 6);
        assert_eq!(note.headings[0].span.line, 6);
        assert_eq!(parse("---\nalias: One\n---\n").aliases, ["One"]);
        assert_eq!(parse("---\naliases: [A, 'B']\n---\n").aliases, ["A", "B"]);
        // Links in frontmatter are properties, not links.
        assert!(parse("---\nup: \"[[x]]\"\n---\n").links.is_empty());
    }

    #[test]
    fn weft_syntax() {
        let weft = Syntax::Weft(Layout::Nested(String::new()));
        let text = "see [Plan] and [Array#C++|vectors], ![a.png] [[Both]]\n\
                    - [ ] task\n\
                    - [x] done[^1] [...] \\[no] [md](x.md) [ref][r] `[code]`\n\
                    [label]: https://x.org\n\
                    on @2026.11.22. (@2026.02.03) me@2026.11.22 @2026.11.223 @2026.02.30 `@2026.11.22`";
        let note = parse_with(text, &weft);
        let got: Vec<_> = note.links.iter().map(|l| (l.kind, l.path.as_str(), l.embed)).collect();
        assert_eq!(
            got,
            [
                (Kind::Wiki, "Plan", false),
                (Kind::Wiki, "Array", false),
                (Kind::Wiki, "a.png", true),
                (Kind::Wiki, "Both", false),
                (Kind::Markdown, "x.md", false),
                (Kind::Day, "2026/11/22", false),
                (Kind::Day, "2026/02/03", false),
            ]
        );
        let plan = &note.links[0];
        assert_eq!((plan.span.start, plan.span.end, plan.path_span.start, plan.path_span.end), (4, 10, 5, 9));
        assert_eq!(note.links[1].subpath[0].text, "C++");
        let day = &note.links[5];
        assert_eq!((day.span.start, day.span.end, day.path_span.start), (3, 14, 4));
        // Obsidian's syntax has neither.
        assert!(parse("[Plan] @2026.11.22").links.is_empty());
    }

    #[test]
    fn writing_links() {
        let weft = Syntax::Weft(Layout::Nested(String::new()));
        assert_eq!(write_link(&weft, "2026/11/22", "2026/11/22.md"), "@2026.11.22");
        assert_eq!(write_link(&weft, "Plan", "Plan.md"), "[Plan]");
        assert_eq!(write_link(&weft, "x", "x.md"), "[[x]]");
        assert_eq!(write_link(&weft, "...", "....md"), "[[...]]");
        assert_eq!(write_link(&Syntax::Obsidian, "2026/11/22", "2026/11/22.md"), "[[2026/11/22]]");
    }

    #[test]
    fn markdown_links() {
        let l = &links("[a](docs/My%20Note.md#Some-Part) [w](https://x.org) ![i](<img 1.png>) [[w]]");
        assert_eq!(l.len(), 3);
        assert_eq!(l[0].kind, Kind::Markdown);
        assert_eq!(l[0].path, "docs/My Note.md");
        assert_eq!(l[0].subpath[0].text, "Some-Part");
        assert!(l[1].embed);
        assert_eq!(l[1].path, "img 1.png");
        assert_eq!(l[2].kind, Kind::Wiki);
    }
}
