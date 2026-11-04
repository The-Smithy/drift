//! reStructuredText, AsciiDoc and plain text, plus the splitting that gives a document with no
//! headings something an agent can point at.

use super::markdown::{self, Heading};
use super::{Extracted, truncate};
use crate::model::Symbol;

// A piece stops at the first paragraph break after this many lines, or at the hard limit if
// there isn't one. Long enough to hold a thought, short enough to be cheap to read.
const TARGET_LINES: usize = 40;
const MAX_LINES: usize = 120;
const MIN_TAIL_LINES: usize = 10;
const MAX_LABEL: usize = 60;

/// Splits lines `start..end` into pieces at paragraph breaks outside code blocks. Each piece is a
/// `(first, last)` pair of indexes, inclusive, with blank lines trimmed from both ends.
pub fn chunks(lines: &[&str], start: usize, end: usize) -> Vec<(usize, usize)> {
    let mut pieces: Vec<(usize, usize)> = Vec::new();
    let mut first = None;
    let mut in_code = false;
    for (at, line) in lines.iter().enumerate().take(end).skip(start) {
        let opening = first.get_or_insert(at);
        let length = at + 1 - *opening;
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code = !in_code;
        }
        let at_break = line.trim().is_empty() && !in_code && length >= TARGET_LINES;
        if at_break || length >= MAX_LINES {
            pieces.push((*opening, at));
            first = None;
        }
    }
    if let Some(opening) = first {
        pieces.push((opening, end.min(lines.len()).saturating_sub(1)));
    }

    let mut trimmed: Vec<(usize, usize)> = pieces
        .into_iter()
        .filter_map(|(mut from, mut to)| {
            while from <= to && lines[from].trim().is_empty() {
                from += 1;
            }
            while to > from && lines[to].trim().is_empty() {
                to -= 1;
            }
            (from <= to).then_some((from, to))
        })
        .collect();

    // A few stray lines read better with what came before them.
    if trimmed.len() >= 2 {
        let (from, to) = trimmed[trimmed.len() - 1];
        if to + 1 - from < MIN_TAIL_LINES {
            trimmed.pop();
            if let Some(previous) = trimmed.last_mut() {
                previous.1 = to;
            }
        }
    }
    trimmed
}

/// Sections for a document with no headings, named after their first line.
pub fn section_symbols(lines: &[&str], start: usize) -> Vec<Symbol> {
    chunks(lines, start, lines.len())
        .into_iter()
        .map(|(from, to)| Symbol {
            name: truncate(&markdown::clean_title(lines[from]), MAX_LABEL),
            kind: "section".to_string(),
            line: from as u32 + 1,
            end_line: Some(to as u32 + 1),
            exported: false,
            is_async: false,
            parent: None,
            decorators: None,
            signature: None,
            doc: None,
        })
        .collect()
}

fn adornment(line: &str) -> Option<char> {
    let line = line.trim_end();
    let marker = line.chars().next()?;
    (line.len() >= 3
        && marker.is_ascii_punctuation()
        && !marker.is_ascii_alphanumeric()
        && line.chars().all(|c| c == marker))
    .then_some(marker)
}

// In reStructuredText a heading is any line underlined with punctuation, and its level is the
// order in which that style of underline first appears in the file.
fn rst_headings(source: &str) -> Vec<Heading> {
    let lines: Vec<&str> = source.lines().collect();
    let mut styles: Vec<(char, bool)> = Vec::new();
    let mut found = Vec::new();
    for at in 0..lines.len().saturating_sub(1) {
        let title = lines[at].trim_end();
        let Some(marker) = adornment(lines[at + 1]) else {
            continue;
        };
        let overlined = at > 0 && adornment(lines[at - 1]) == Some(marker);
        let indented = title.starts_with([' ', '\t']);
        if title.is_empty()
            || adornment(title).is_some()
            || (indented && !overlined)
            || lines[at + 1].trim_end().chars().count() < title.trim().chars().count()
        {
            continue;
        }
        let style = (marker, overlined);
        let level = styles
            .iter()
            .position(|known| *known == style)
            .unwrap_or_else(|| {
                styles.push(style);
                styles.len() - 1
            });
        let title = markdown::clean_title(title);
        if !title.is_empty() {
            found.push(Heading {
                level: level + 1,
                title,
                line: at as u32 + 1,
            });
        }
    }
    found
}

fn adoc_headings(source: &str) -> Vec<Heading> {
    let mut found = Vec::new();
    let mut block: Option<&str> = None;
    for (at, line) in source.lines().enumerate() {
        let trimmed = line.trim_end();
        // A run of four or more marks opens or closes a listing, example or passthrough block.
        if adornment(trimmed).is_some() && trimmed.len() >= 4 {
            match block {
                Some(open) if open == trimmed => block = None,
                None => block = Some(trimmed),
                _ => {}
            }
            continue;
        }
        if block.is_some() {
            continue;
        }
        let level = trimmed.bytes().take_while(|&b| b == b'=').count();
        let Some(rest) = trimmed.get(level..).filter(|_| (1..=6).contains(&level)) else {
            continue;
        };
        if !rest.starts_with(' ') {
            continue;
        }
        let title = markdown::clean_title(rest);
        if !title.is_empty() {
            found.push(Heading {
                level,
                title,
                line: at as u32 + 1,
            });
        }
    }
    found
}

// Keeps word lists, dependency lists and number dumps out of the documents.
fn is_prose(source: &str) -> bool {
    let lines: Vec<&str> = source
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(200)
        .collect();
    if lines.is_empty() {
        return false;
    }
    let words: usize = lines
        .iter()
        .map(|line| line.split_whitespace().count())
        .sum();
    let letters = lines
        .iter()
        .flat_map(|line| line.chars())
        .filter(|c| c.is_alphabetic() || c.is_whitespace())
        .count();
    let characters: usize = lines.iter().map(|line| line.chars().count()).sum();
    words >= lines.len() * 3 && letters * 10 >= characters * 7
}

pub fn extract_rst(source: &str) -> Extracted {
    markdown::document("rst", source, rst_headings(source), 0)
}

pub fn extract_adoc(source: &str) -> Extracted {
    markdown::document("asciidoc", source, adoc_headings(source), 0)
}

pub fn extract_text(source: &str) -> Extracted {
    let mut extracted = markdown::document("text", source, Vec::new(), 0);
    if !is_prose(source) {
        extracted.symbols.clear();
    }
    extracted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(extracted: &Extracted) -> Vec<(String, u32, u32, Option<String>)> {
        extracted
            .symbols
            .iter()
            .map(|s| {
                (
                    s.name.clone(),
                    s.line,
                    s.end_line.unwrap(),
                    s.parent.clone(),
                )
            })
            .collect()
    }

    #[test]
    fn rst_levels_follow_the_order_underlines_appear() {
        let source = "Guide\n=====\n\nintro\n\nInstall\n-------\n\ntext\n\nUsage\n-----\n\nmore\n";
        assert_eq!(
            names(&extract_rst(source)),
            vec![
                ("Guide".to_string(), 1, 14, None),
                ("Install".to_string(), 6, 10, Some("Guide".to_string())),
                ("Usage".to_string(), 11, 14, Some("Guide".to_string())),
            ]
        );
    }

    #[test]
    fn rst_overlined_titles_are_their_own_style() {
        let source = "=====\nTitle\n=====\n\nPart\n====\n";
        let found = names(&extract_rst(source));
        assert_eq!(found[0].0, "Title");
        assert_eq!(found[1].3.as_deref(), Some("Title"));
    }

    #[test]
    fn rst_needs_an_underline_as_long_as_the_title() {
        let source = "A long title\n---\n\n    code\n    ----\n";
        assert!(rst_headings(source).is_empty());
    }

    #[test]
    fn adoc_headings_skip_listing_blocks() {
        let source = "= Title\n\n== One\ntext\n----\n== not a heading\n----\n=== Deep\n";
        let found = names(&extract_adoc(source));
        let titles: Vec<&str> = found.iter().map(|h| h.0.as_str()).collect();
        assert_eq!(titles, vec!["Title", "One", "Deep"]);
    }

    #[test]
    fn text_splits_into_sections_named_by_first_line() {
        let mut source = String::new();
        for paragraph in 0..12 {
            source.push_str(&format!("Paragraph {paragraph} has some words here\n"));
            source.push_str(&"more words in this line\n".repeat(9));
            source.push('\n');
        }
        let extracted = extract_text(&source);
        assert_eq!(extracted.symbols[0].kind, "section");
        assert_eq!(extracted.symbols[0].name, "Paragraph 0 has some words here");
        assert!(extracted.symbols.len() >= 2);
        // Every non-blank line belongs to exactly one piece.
        let mut next = 1;
        for symbol in &extracted.symbols {
            assert!(symbol.line >= next);
            next = symbol.end_line.unwrap() + 1;
        }
    }

    #[test]
    fn lists_are_not_prose() {
        assert!(
            extract_text("flask\nrequests==2.0\nblack\n")
                .symbols
                .is_empty()
        );
        assert!(extract_text("1 2 3 4 5\n6 7 8 9 0\n").symbols.is_empty());
        assert!(
            !extract_text("This line is a sentence about the tool.\n")
                .symbols
                .is_empty()
        );
    }

    #[test]
    fn code_blocks_are_not_split_at_their_blank_lines() {
        let mut lines: Vec<String> = vec!["```".to_string()];
        for _ in 0..30 {
            lines.push("code".to_string());
            lines.push(String::new());
        }
        lines.push("```".to_string());
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        assert_eq!(chunks(&lines, 0, lines.len()), vec![(0, 61)]);
    }

    #[test]
    fn a_long_block_stops_at_the_hard_limit() {
        let lines = vec!["word"; 300];
        let pieces = chunks(&lines, 0, 300);
        assert!(pieces.iter().all(|(from, to)| to + 1 - from <= MAX_LINES));
        assert_eq!(pieces.last().unwrap().1, 299);
    }
}
