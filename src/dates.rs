//! `@` completion for daily notes: `@today`, `@fri`, `@11.15` or
//! `@2026.11.15` becomes a plain `[[link]]` to that day's note.
//!
//! Nothing new is written into notes; `@` is only a way of typing the link.
//! Where daily notes live is read from the vault, so there is nothing to set.

use jiff::civil::{Date, Weekday};

/// How the vault names its daily notes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Layout {
    /// `<folder>/YYYY/MM/DD.md`
    Nested(String),
    /// `<folder>/YYYY-MM-DD.md`, Obsidian's default.
    Flat(String),
}

impl Layout {
    /// The layout most daily notes in `paths` use; Obsidian's default
    /// (`YYYY-MM-DD.md` at the root) when there are none.
    pub fn detect<'a>(paths: impl Iterator<Item = &'a String>) -> Layout {
        let mut counts: Vec<(Layout, usize)> = Vec::new();
        for path in paths {
            let Some(layout) = Layout::of(path) else { continue };
            match counts.iter_mut().find(|(l, _)| *l == layout) {
                Some((_, n)) => *n += 1,
                None => counts.push((layout, 1)),
            }
        }
        counts.into_iter().max_by_key(|(_, n)| *n).map_or(Layout::Flat(String::new()), |(l, _)| l)
    }

    fn of(path: &str) -> Option<Layout> {
        let stem = path.strip_suffix(".md")?;
        let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
        let folder = |s: &str| s.strip_suffix('/').unwrap_or(s).to_string();
        let (rest, name) = stem.rsplit_once('/').unwrap_or(("", stem));
        if let [y, m, d] = name.split('-').collect::<Vec<_>>()[..]
            && digits(y, 4)
            && digits(m, 2)
            && digits(d, 2)
        {
            return Some(Layout::Flat(rest.to_string()));
        }
        let parts: Vec<&str> = stem.rsplitn(4, '/').collect();
        if let [d, m, y, ..] = parts[..]
            && digits(y, 4)
            && digits(m, 2)
            && digits(d, 2)
        {
            let prefix = stem.len() - y.len() - m.len() - d.len() - 2;
            return Some(Layout::Nested(folder(&stem[..prefix])));
        }
        None
    }

    /// The note for `date`, from the vault root.
    pub fn path(&self, date: Date) -> String {
        let join = |folder: &str, name: String| if folder.is_empty() { name } else { format!("{folder}/{name}") };
        match self {
            Layout::Nested(f) => join(f, date.strftime("%Y/%m/%d.md").to_string()),
            Layout::Flat(f) => join(f, date.strftime("%Y-%m-%d.md").to_string()),
        }
    }
}

const WORDS: [(&str, i8); 3] = [("today", 0), ("yesterday", -1), ("tomorrow", 1)];
const WEEKDAYS: [(&str, Weekday); 7] = [
    ("monday", Weekday::Monday),
    ("tuesday", Weekday::Tuesday),
    ("wednesday", Weekday::Wednesday),
    ("thursday", Weekday::Thursday),
    ("friday", Weekday::Friday),
    ("saturday", Weekday::Saturday),
    ("sunday", Weekday::Sunday),
];

/// The days `query` (the text after `@`) can mean, each with a word for it
/// when it has one. Nothing when it is not a date being typed, such as the
/// rest of an email address.
pub fn days(query: &str, today: Date) -> Vec<(Date, Option<&'static str>)> {
    let q = query.to_lowercase();
    if q.is_empty() {
        return WORDS.iter().filter_map(|&(w, n)| Some((today.checked_add(jiff::Span::new().days(n)).ok()?, Some(w)))).collect();
    }
    if q.bytes().all(|b| b.is_ascii_alphabetic()) {
        let mut out = Vec::new();
        for &(word, n) in &WORDS {
            if word.starts_with(&q) {
                out.extend(today.checked_add(jiff::Span::new().days(n)).ok().map(|d| (d, Some(word))));
            }
        }
        for &(word, weekday) in &WEEKDAYS {
            if word.starts_with(&q) {
                // The latest such day, today included.
                let back = (today.weekday().to_monday_zero_offset() - weekday.to_monday_zero_offset()).rem_euclid(7);
                out.extend(today.checked_sub(jiff::Span::new().days(back)).ok().map(|d| (d, Some(word))));
            }
        }
        return out;
    }
    if !q.bytes().all(|b| b.is_ascii_digit() || b"./-".contains(&b)) {
        return Vec::new();
    }
    let nums: Vec<i32> = q.split(['.', '/', '-']).filter(|s| !s.is_empty()).filter_map(|s| s.parse().ok()).collect();
    let date = match nums[..] {
        [d] => Date::new(today.year(), today.month(), d as i8),
        [m, d] => Date::new(today.year(), m as i8, d as i8),
        [y, m, d] => Date::new(if y < 100 { 2000 + y as i16 } else { y as i16 }, m as i8, d as i8),
        _ => return Vec::new(),
    };
    date.ok().map(|d| (d, None)).into_iter().collect()
}

/// Today in the system's time zone.
pub fn today() -> Date {
    jiff::Zoned::now().date()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    #[test]
    fn layouts() {
        let paths: Vec<String> =
            ["2026/11/14.md", "2026/11/15.md", "Notes/2025-01-02.md", "Plan.md"].map(String::from).into();
        let layout = Layout::detect(paths.iter());
        assert_eq!(layout, Layout::Nested(String::new()));
        assert_eq!(layout.path(date(2026, 3, 5)), "2026/03/05.md");
        assert_eq!(Layout::detect([].iter()).path(date(2026, 3, 5)), "2026-03-05.md");
        assert_eq!(Layout::of("Journal/2026/11/14.md"), Some(Layout::Nested("Journal".into())));
        assert_eq!(Layout::of("Daily/2026-11-14.md"), Some(Layout::Flat("Daily".into())));
        assert_eq!(Layout::of("2026/1/14.md"), None);
    }

    #[test]
    fn queries() {
        let today = date(2026, 11, 21); // a Saturday
        let first = |q: &str| days(q, today).first().map(|d| d.0);
        assert_eq!(days("", today).len(), 3);
        assert_eq!(first("yes"), Some(date(2026, 11, 20)));
        assert_eq!(first("fri"), Some(date(2026, 11, 20)));
        assert_eq!(first("sat"), Some(today));
        assert_eq!(first("sun"), Some(date(2026, 11, 15)));
        assert_eq!(days("t", today).len(), 4); // today, tomorrow, tuesday, thursday
        assert_eq!(first("11.15"), Some(date(2026, 11, 15)));
        assert_eq!(first("2025.1.2"), Some(date(2025, 1, 2)));
        assert_eq!(first("25/1/2"), Some(date(2025, 1, 2)));
        assert_eq!(first("3"), Some(date(2026, 11, 3)));
        assert_eq!(first("11."), Some(date(2026, 11, 11)));
        assert_eq!(first("2.30"), None);
        assert!(days("gmail", today).is_empty());
        assert!(days("x.com", today).is_empty());
    }
}
