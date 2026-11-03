use std::sync::LazyLock;

use regex::Regex;

use super::{Extracted, line_count};
use crate::model::Symbol;

const NAME: &str = r#"(?P<name>(?:"[^"]+"|`[^`]+`|\[[^\]]+\]|[\w$]+)(?:\.(?:"[^"]+"|`[^`]+`|\[[^\]]+\]|[\w$]+))*)"#;

static PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (
            r"CREATE\s+(?:OR\s+REPLACE\s+)?(?:(?:GLOBAL\s+|LOCAL\s+)?TEMP(?:ORARY)?\s+|UNLOGGED\s+)?TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?",
            "table",
        ),
        (
            r"CREATE\s+(?:OR\s+REPLACE\s+)?(?:TEMP(?:ORARY)?\s+)?(?:MATERIALIZED\s+)?VIEW\s+(?:IF\s+NOT\s+EXISTS\s+)?",
            "view",
        ),
        (r"CREATE\s+(?:OR\s+REPLACE\s+)?FUNCTION\s+", "function"),
        (r"CREATE\s+(?:OR\s+REPLACE\s+)?PROC(?:EDURE)?\s+", "procedure"),
        (
            r"CREATE\s+(?:OR\s+REPLACE\s+)?(?:CONSTRAINT\s+)?TRIGGER\s+",
            "trigger",
        ),
        (
            r"CREATE\s+(?:UNIQUE\s+)?INDEX\s+(?:CONCURRENTLY\s+)?(?:IF\s+NOT\s+EXISTS\s+)?",
            "const",
        ),
        (r"CREATE\s+TYPE\s+", "type"),
        (r"CREATE\s+POLICY\s+", "const"),
    ]
    .into_iter()
    .map(|(prefix, kind)| {
        let pattern = format!(r"(?i)\b{prefix}{NAME}");
        (Regex::new(&pattern).expect("valid SQL pattern"), kind)
    })
    .collect()
});

pub fn extract(source: &str) -> Extracted {
    let masked = mask_comments(source);
    let mut symbols = Vec::new();

    for (pattern, kind) in PATTERNS.iter() {
        for captures in pattern.captures_iter(&masked) {
            let raw = &captures["name"];
            // `CREATE INDEX ON t (...)` has no name; the grammar's next word is not one.
            if raw.eq_ignore_ascii_case("on") {
                continue;
            }
            let start = captures.get(0).map_or(0, |m| m.start());
            let line = line_at(&masked, start);
            let end = statement_end(&masked, start);
            let signature = masked[start..end]
                .lines()
                .next()
                .map(|first| first.trim().trim_end_matches(['(', ' ']).to_string())
                .filter(|first| !first.is_empty());
            symbols.push(Symbol {
                name: clean_name(raw),
                kind: kind.to_string(),
                line,
                end_line: Some(line_at(&masked, end.saturating_sub(1))),
                exported: true,
                is_async: false,
                parent: None,
                decorators: None,
                signature,
                doc: None,
            });
        }
    }
    symbols.sort_by_key(|s| s.line);

    Extracted {
        lang: "sql",
        lines: line_count(source),
        symbols,
        imports: Vec::new(),
        tests: None,
    }
}

fn line_at(text: &str, offset: usize) -> u32 {
    text[..offset].bytes().filter(|&b| b == b'\n').count() as u32 + 1
}

// One past the statement's `;`, skipping any inside quotes or a `$tag$ ... $tag$` function body.
fn statement_end(masked: &str, start: usize) -> usize {
    let bytes = masked.as_bytes();
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b';' => return i + 1,
            b'\'' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'\'' {
                    i += 1;
                }
            }
            b'$' => {
                let tag_end = bytes[i + 1..]
                    .iter()
                    .position(|&b| !(b.is_ascii_alphanumeric() || b == b'_'))
                    .map(|length| i + 1 + length);
                if let Some(tag_end) = tag_end
                    && bytes[tag_end] == b'$'
                {
                    let tag = &masked[i..=tag_end];
                    if let Some(close) = masked[tag_end + 1..].find(tag) {
                        i = tag_end + 1 + close + tag.len();
                        continue;
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    bytes.len()
}

pub fn reference_lines(source: &str, name: &str) -> Vec<u32> {
    let is_word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let mut lines: Vec<u32> = Vec::new();
    for (number, line) in mask_comments(source).lines().enumerate() {
        let found = line.match_indices(name).any(|(at, _)| {
            !line[..at].chars().next_back().is_some_and(is_word)
                && !line[at + name.len()..].chars().next().is_some_and(is_word)
        });
        if found {
            lines.push(number as u32 + 1);
        }
    }
    lines
}

fn clean_name(raw: &str) -> String {
    raw.chars()
        .filter(|c| !matches!(c, '"' | '`' | '[' | ']'))
        .collect()
}

// Blanks comments in place so byte offsets, and therefore line numbers, stay correct.
fn mask_comments(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = bytes.to_vec();
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'\'' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'\'' {
                    i += 1;
                }
                i += 1;
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    out[i] = b' ';
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let mut depth = 0;
                while i < bytes.len() {
                    if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        out[i] = b' ';
                        out[i + 1] = b' ';
                        i += 2;
                    } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        out[i] = b' ';
                        out[i + 1] = b' ';
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        if bytes[i] != b'\n' {
                            out[i] = b' ';
                        }
                        i += 1;
                    }
                }
            }
            _ => i += 1,
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| source.to_string())
}

#[cfg(test)]
mod tests {
    use crate::extract::extract;

    fn symbols(source: &str) -> Vec<(String, String, u32)> {
        extract("sql", source)
            .unwrap()
            .symbols
            .into_iter()
            .map(|s| (s.name, s.kind, s.line))
            .collect()
    }

    fn entry(name: &str, kind: &str, line: u32) -> (String, String, u32) {
        (name.to_string(), kind.to_string(), line)
    }

    #[test]
    fn create_statements_of_every_kind() {
        let found = symbols(
            "CREATE TABLE users (id int);\n\
             create or replace view v_users as select 1;\n\
             CREATE MATERIALIZED VIEW IF NOT EXISTS mv as select 1;\n\
             CREATE FUNCTION f() returns int;\n\
             CREATE PROCEDURE p();\n\
             CREATE OR REPLACE TRIGGER t before insert on users;\n\
             CREATE UNIQUE INDEX CONCURRENTLY idx on users (id);\n\
             CREATE TYPE mood AS ENUM ('a');\n",
        );
        assert_eq!(
            found,
            vec![
                entry("users", "table", 1),
                entry("v_users", "view", 2),
                entry("mv", "view", 3),
                entry("f", "function", 4),
                entry("p", "procedure", 5),
                entry("t", "trigger", 6),
                entry("idx", "const", 7),
                entry("mood", "type", 8),
            ]
        );
    }

    #[test]
    fn quoted_and_schema_qualified_names() {
        let found = symbols(
            "CREATE TABLE \"public\".\"users\" (id int);\n\
             CREATE POLICY \"Users can read their rows\" ON users;\n\
             CREATE TABLE public.orders (id int);\n",
        );
        assert_eq!(
            found,
            vec![
                entry("public.users", "table", 1),
                entry("Users can read their rows", "const", 2),
                entry("public.orders", "table", 3),
            ]
        );
    }

    #[test]
    fn comments_are_ignored_but_strings_do_not_start_them() {
        let found = symbols(
            "-- CREATE TABLE old (id int);\n\
             /* CREATE TABLE older (id int);\n   CREATE TABLE oldest (id int); */\n\
             INSERT INTO t VALUES ('-- not a comment');\n\
             CREATE TABLE kept (id int);\n",
        );
        assert_eq!(found, vec![entry("kept", "table", 5)]);
    }

    #[test]
    fn statements_end_at_their_semicolon_and_skip_function_bodies() {
        let source = [
            "CREATE TABLE a (",
            "  id int",
            ");",
            "CREATE FUNCTION f() RETURNS int AS $body$",
            "BEGIN",
            "  x := 1;",
            "END;",
            "$body$ LANGUAGE sql;",
            "",
        ]
        .join("\n");
        let symbols = extract("sql", &source).unwrap().symbols;
        assert_eq!((symbols[0].line, symbols[0].end_line), (1, Some(3)));
        assert_eq!(symbols[0].signature.as_deref(), Some("CREATE TABLE a"));
        assert_eq!((symbols[1].line, symbols[1].end_line), (4, Some(8)));
    }

    #[test]
    fn references_match_whole_words_outside_comments() {
        let source = "-- users here\nSELECT * FROM users;\nSELECT * FROM users_old;\n";
        assert_eq!(super::reference_lines(source, "users"), vec![2]);
    }

    #[test]
    fn unnamed_index_is_skipped() {
        assert!(symbols("CREATE INDEX ON users (id);\n").is_empty());
    }
}
