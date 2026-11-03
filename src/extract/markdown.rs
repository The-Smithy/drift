//! Markdown headings, so an agent can read one section of a long document.

use std::sync::LazyLock;

use regex::Regex;

use super::{Extracted, MAX_SIGNATURE, line_count, truncate};
use crate::model::Symbol;

static LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!?\[([^\]]*)\]\([^)]*\)").expect("valid regex"));

struct Heading {
    level: usize,
    title: String,
    line: u32,
}

// Up to three spaces of indent is still a heading or fence; four or more is a code block.
fn unindented(line: &str) -> Option<&str> {
    let body = line.trim_start_matches(' ');
    (line.len() - body.len() <= 3).then_some(body)
}

fn clean_title(raw: &str) -> String {
    let linked = LINK.replace_all(raw, "$1");
    linked
        .replace(['`', '*'], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn atx_heading(line: &str) -> Option<(usize, String)> {
    let body = unindented(line)?;
    let level = body.bytes().take_while(|&b| b == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &body[level..];
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    // A closing run of #s only counts when a space sets it apart, as in `## Title ##`.
    let trimmed = rest.trim();
    let without_closing = trimmed.trim_end_matches('#');
    let title = if without_closing.is_empty() || without_closing.ends_with([' ', '\t']) {
        without_closing
    } else {
        trimmed
    };
    let title = clean_title(title);
    (!title.is_empty()).then_some((level, title))
}

fn setext_level(line: &str) -> Option<usize> {
    let body = unindented(line)?.trim_end();
    if !body.is_empty() && body.bytes().all(|b| b == b'=') {
        Some(1)
    } else if !body.is_empty() && body.bytes().all(|b| b == b'-') {
        Some(2)
    } else {
        None
    }
}

// Whether a line could be the text of a heading underlined on the next line.
fn is_plain_text(line: &str) -> bool {
    let Some(body) = unindented(line) else {
        return false;
    };
    !body.trim().is_empty()
        && !body.starts_with(['>', '#', '|', '-', '*', '+'])
        && !body.chars().next().is_some_and(|c| c.is_ascii_digit())
        && !body.contains(" | ")
}

fn fence_of(line: &str) -> Option<(u8, usize)> {
    let body = unindented(line)?;
    let marker = *body.as_bytes().first()?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let length = body.bytes().take_while(|&b| b == marker).count();
    (length >= 3).then_some((marker, length))
}

// Where the body starts, past any YAML or TOML front matter.
fn skip_front_matter(lines: &[&str]) -> usize {
    let Some(marker) = lines.first().map(|line| line.trim_end()) else {
        return 0;
    };
    if marker != "---" && marker != "+++" {
        return 0;
    }
    lines
        .iter()
        .skip(1)
        .position(|line| line.trim_end() == marker)
        .map_or(0, |closing| closing + 2)
}

fn headings(source: &str) -> Vec<Heading> {
    let lines: Vec<&str> = source.lines().collect();
    let mut found = Vec::new();
    let mut fence: Option<(u8, usize)> = None;
    let mut in_comment = false;

    for (at, line) in lines.iter().enumerate().skip(skip_front_matter(&lines)) {
        if in_comment {
            in_comment = !line.contains("-->");
            continue;
        }
        if let Some((marker, length)) = fence {
            let closes = fence_of(line).is_some_and(|(m, l)| {
                m == marker
                    && l >= length
                    && unindented(line).is_some_and(|body| body.trim_end().bytes().all(|b| b == m))
            });
            if closes {
                fence = None;
            }
            continue;
        }
        if let Some(opened) = fence_of(line) {
            fence = Some(opened);
            continue;
        }
        if line.trim_start().starts_with("<!--") && !line.contains("-->") {
            in_comment = true;
            continue;
        }
        if let Some((level, title)) = atx_heading(line) {
            found.push(Heading {
                level,
                title,
                line: at as u32 + 1,
            });
            continue;
        }
        if at > 0
            && is_plain_text(lines[at - 1])
            && let Some(level) = setext_level(line)
        {
            let title = clean_title(lines[at - 1]);
            if !title.is_empty() {
                found.push(Heading {
                    level,
                    title,
                    line: at as u32,
                });
            }
        }
    }
    found
}

pub fn extract(source: &str) -> Extracted {
    let lines = line_count(source);
    let last_with_text = source
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(at, _)| at as u32 + 1)
        .last()
        .unwrap_or(lines);

    let found = headings(source);
    let symbols = found
        .iter()
        .enumerate()
        .map(|(at, heading)| {
            // A section runs until the next heading at its own level or above.
            let end = found[at + 1..]
                .iter()
                .find(|next| next.level <= heading.level)
                .map_or(last_with_text, |next| next.line - 1);
            let parent = found[..at]
                .iter()
                .rev()
                .find(|earlier| earlier.level < heading.level)
                .map(|earlier| earlier.title.clone());
            Symbol {
                name: heading.title.clone(),
                kind: "heading".to_string(),
                line: heading.line,
                end_line: Some(end.max(heading.line)),
                exported: false,
                is_async: false,
                parent,
                decorators: None,
                signature: Some(truncate(
                    &format!("{} {}", "#".repeat(heading.level), heading.title),
                    MAX_SIGNATURE,
                )),
                doc: None,
            }
        })
        .collect();

    Extracted {
        lang: "markdown",
        lines,
        symbols,
        imports: Vec::new(),
        tests: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outline(source: &str) -> Vec<(String, u32, u32, Option<String>)> {
        extract(source)
            .symbols
            .into_iter()
            .map(|s| (s.name, s.line, s.end_line.unwrap(), s.parent))
            .collect()
    }

    #[test]
    fn sections_run_to_the_next_heading_at_their_level() {
        let source = "# Top\nintro\n\n## One\ntext\n\n### Deep\nmore\n\n## Two\nend\n";
        assert_eq!(
            outline(source),
            vec![
                ("Top".to_string(), 1, 11, None),
                ("One".to_string(), 4, 9, Some("Top".to_string())),
                ("Deep".to_string(), 7, 9, Some("One".to_string())),
                ("Two".to_string(), 10, 11, Some("Top".to_string())),
            ]
        );
    }

    #[test]
    fn ignores_hashes_in_code_and_comments() {
        let source = "# Real\n```bash\n# a shell comment\n```\n~~~\n# also code\n~~~\n    # indented code\n<!--\n# hidden\n-->\n## Next\n";
        let names: Vec<_> = outline(source).into_iter().map(|h| h.0).collect();
        assert_eq!(names, vec!["Real", "Next"]);
    }

    #[test]
    fn a_longer_fence_needs_a_longer_close() {
        let source = "````\n```\n# inside\n```\n# still inside\n````\n# Out\n";
        let names: Vec<_> = outline(source).into_iter().map(|h| h.0).collect();
        assert_eq!(names, vec!["Out"]);
    }

    #[test]
    fn skips_front_matter() {
        let source = "---\ntitle: x\n# not a heading\n---\n# Real\n";
        let found = outline(source);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].0.as_str(), found[0].1), ("Real", 5));
    }

    #[test]
    fn reads_underlined_headings_but_not_rules() {
        let source = "Title\n=====\n\ntext\n\n---\n\nSub\n---\n- item\n---\n";
        let found = outline(source);
        let names: Vec<_> = found.iter().map(|h| h.0.as_str()).collect();
        assert_eq!(names, vec!["Title", "Sub"]);
        assert_eq!((found[0].1, found[1].1), (1, 8));
    }

    #[test]
    fn tidies_titles() {
        let names: Vec<_> = outline(
            "# [Docs](https://x.y) and `code`\n## **Bold**\n### Closed ###\n#### C#\n#nospace\n#\n",
        )
        .into_iter()
        .map(|h| h.0)
        .collect();
        assert_eq!(names, vec!["Docs and code", "Bold", "Closed", "C#"]);
    }

    #[test]
    fn handles_crlf_and_empty_files() {
        let found = outline("# A\r\ntext\r\n## B\r\n");
        assert_eq!(found[1].0, "B");
        assert!(outline("").is_empty());
    }
}
