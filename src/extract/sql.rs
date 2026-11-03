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
            let line = masked[..start].bytes().filter(|&b| b == b'\n').count() as u32 + 1;
            symbols.push(Symbol {
                name: clean_name(raw),
                kind: kind.to_string(),
                line,
                end_line: None,
                exported: true,
                is_async: false,
                parent: None,
                decorators: None,
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
    fn unnamed_index_is_skipped() {
        assert!(symbols("CREATE INDEX ON users (id);\n").is_empty());
    }
}
