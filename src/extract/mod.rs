mod c_family;
mod csharp;
mod data;
mod gdscript;
mod lua;
mod markdown;
mod python;
mod rust;
mod sql;
mod typescript;

pub use rust::split_top_level;
use tree_sitter::{Language, Node, Parser, Tree};

use crate::model::{Import, ImportNames, Symbol, TestFile};

pub const UNKNOWN_FRAMEWORK: &str = "unknown";

const MAX_SIGNATURE: usize = 200;
const MAX_DOC: usize = 120;

pub struct Extracted {
    pub lang: &'static str,
    pub lines: u32,
    pub symbols: Vec<Symbol>,
    pub imports: Vec<Import>,
    pub tests: Option<TestFile>,
}

pub fn is_supported(extension: &str) -> bool {
    matches!(
        extension,
        "ts" | "tsx"
            | "mts"
            | "cts"
            | "js"
            | "jsx"
            | "mjs"
            | "cjs"
            | "py"
            | "rs"
            | "sql"
            | "md"
            | "mdx"
            | "markdown"
            | "json"
            | "jsonc"
            | "yaml"
            | "yml"
            | "lua"
            | "gd"
            | "cs"
            | "c"
            | "h"
            | "cpp"
            | "cc"
            | "cxx"
            | "hpp"
            | "hh"
            | "hxx"
            | "ipp"
            | "inl"
    )
}

pub fn extract(extension: &str, source: &str) -> Option<Extracted> {
    match extension {
        "ts" | "mts" | "cts" => typescript::extract(
            source,
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            "typescript",
        ),
        "tsx" => typescript::extract(
            source,
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            "typescript",
        ),
        "js" | "jsx" | "mjs" | "cjs" => typescript::extract(
            source,
            tree_sitter_javascript::LANGUAGE.into(),
            "javascript",
        ),
        "py" => python::extract(source),
        "rs" => rust::extract(source),
        "sql" => Some(sql::extract(source)),
        "md" | "mdx" | "markdown" => Some(markdown::extract(source)),
        "json" | "jsonc" => data::extract_json(source),
        "yaml" | "yml" => data::extract_yaml(source),
        "lua" => lua::extract(source),
        "gd" => gdscript::extract(source),
        "cs" => csharp::extract(source),
        "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "ipp" | "inl" => {
            c_family::extract(extension, source)
        }
        _ => None,
    }
}

fn parse(source: &str, language: Language) -> Option<Tree> {
    #[cfg(target_env = "musl")]
    crate::allocator::use_mimalloc_in_tree_sitter();

    let mut parser = Parser::new();
    parser.set_language(&language).ok()?;
    parser.parse(source, None)
}

fn line_count(source: &str) -> u32 {
    source.bytes().filter(|&b| b == b'\n').count() as u32 + 1
}

fn text<'a>(node: Node, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

fn start_line(node: Node) -> u32 {
    node.start_position().row as u32 + 1
}

fn named_children<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn has_child_kind(node: Node, kind: &str) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor).any(|c| c.kind() == kind)
}

fn symbol(
    source: &str,
    name: &str,
    kind: &str,
    node: Node,
    exported: bool,
    is_async: bool,
    parent: Option<&str>,
) -> Symbol {
    Symbol {
        name: name.to_string(),
        kind: kind.to_string(),
        line: start_line(node),
        end_line: Some(node.end_position().row as u32 + 1),
        exported,
        is_async,
        parent: parent.map(str::to_string),
        decorators: None,
        signature: signature_of(node, kind, source),
        doc: doc_of(node, source),
    }
}

// Where a definition's header stops and its body or value begins.
fn header_end(node: Node) -> Option<usize> {
    if let Some(body) = node.child_by_field_name("body") {
        return Some(body.start_byte());
    }
    let value = node
        .child_by_field_name("value")
        .or_else(|| node.child_by_field_name("right"));
    if let Some(value) = value {
        return Some(
            value
                .child_by_field_name("body")
                .map_or(value.start_byte(), |body| body.start_byte()),
        );
    }
    named_children(node)
        .into_iter()
        .find(|child| child.kind() == "variable_declarator")
        .and_then(header_end)
}

fn signature_of(node: Node, kind: &str, source: &str) -> Option<String> {
    let end = match kind {
        "type" | "macro" => None,
        _ => header_end(node),
    }
    .unwrap_or_else(|| node.end_byte());
    header_text(&source[node.start_byte()..end])
}

// Whitespace squeezed, and the punctuation that leads into a body removed.
fn header_text(raw: &str) -> Option<String> {
    let normalized = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut header = normalized.as_str();
    while let Some(shorter) = ["=>", "{", ":", "=", ";"]
        .iter()
        .find_map(|tail| header.strip_suffix(tail))
    {
        header = shorter.trim_end();
    }
    (!header.is_empty()).then(|| truncate(header, MAX_SIGNATURE))
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let kept: String = text.chars().take(limit).collect();
    format!("{kept}…")
}

fn is_comment(node: Node) -> bool {
    matches!(node.kind(), "comment" | "line_comment" | "block_comment")
}

fn doc_of(node: Node, source: &str) -> Option<String> {
    if let Some(docstring) = python_docstring(node, source) {
        return Some(docstring);
    }
    // Comments sit before the outermost wrapper: `export`, a decorated definition, attributes.
    let anchor = match node.parent() {
        Some(parent) if matches!(parent.kind(), "export_statement" | "decorated_definition") => {
            parent
        }
        _ => node,
    };
    let mut top = None;
    let mut below = anchor;
    let mut current = anchor.prev_named_sibling();
    while let Some(sibling) = current {
        if sibling.kind() == "attribute_item" {
            below = sibling;
            current = sibling.prev_named_sibling();
            continue;
        }
        let adjacent = sibling.end_position().row + 1 >= below.start_position().row;
        if !is_comment(sibling) || !adjacent {
            break;
        }
        // Rust's plain `//` comments aren't documentation.
        let raw = text(sibling, source);
        if sibling.kind() != "comment" && !(raw.starts_with("///") || raw.starts_with("/**")) {
            break;
        }
        top = Some(sibling);
        below = sibling;
        current = sibling.prev_named_sibling();
    }
    first_comment_line(text(top?, source))
}

fn python_docstring(node: Node, source: &str) -> Option<String> {
    if !matches!(node.kind(), "function_definition" | "class_definition") {
        return None;
    }
    let body = node.child_by_field_name("body")?;
    let first = named_children(body)
        .into_iter()
        .find(|child| !is_comment(*child))?;
    if first.kind() != "expression_statement" {
        return None;
    }
    let string = named_children(first).into_iter().next()?;
    if string.kind() != "string" {
        return None;
    }
    first_comment_line(text(string, source).trim_matches(['"', '\''].as_slice()))
}

fn first_comment_line(raw: &str) -> Option<String> {
    let without_tags = raw.replace("<summary>", "").replace("</summary>", "");
    without_tags
        .lines()
        .map(|line| {
            line.trim()
                .trim_start_matches(['/', '*', '#', '!', '-'])
                .trim_end_matches(['*', '/'])
                .trim()
        })
        .find(|line| !line.is_empty() && !line.starts_with("eslint") && !line.starts_with('@'))
        .map(|line| truncate(line, MAX_DOC))
}

// Lines where `name` is used as code. Comments and strings aren't identifiers, so they never match.
pub fn reference_lines(extension: &str, source: &str, name: &str) -> Vec<u32> {
    let language: Language = match extension {
        "ts" | "mts" | "cts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "js" | "jsx" | "mjs" | "cjs" => tree_sitter_javascript::LANGUAGE.into(),
        "py" => tree_sitter_python::LANGUAGE.into(),
        "rs" => tree_sitter_rust::LANGUAGE.into(),
        "lua" => tree_sitter_lua::LANGUAGE.into(),
        "gd" => tree_sitter_gdscript::LANGUAGE.into(),
        "cs" => tree_sitter_c_sharp::LANGUAGE.into(),
        "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "ipp" | "inl" => {
            c_family::language_for(extension, source).0
        }
        "sql" => return sql::reference_lines(source, name),
        _ => return Vec::new(),
    };
    let Some(tree) = parse(source, language) else {
        return Vec::new();
    };

    let mut lines = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "import_statement"
            | "import_from_statement"
            | "future_import_statement"
            | "use_declaration"
            | "using_directive"
            | "preproc_include"
            | "extern_crate_declaration"
            | "export_clause"
            | "comment"
            | "line_comment"
            | "block_comment" => continue,
            _ => {}
        }
        if node.child_count() == 0 {
            // `const { load } = api` uses `load` through a pattern rather than a plain identifier.
            let kind = node.kind();
            let is_name = kind.ends_with("identifier") || kind.ends_with("identifier_pattern");
            if is_name && text(node, source) == name {
                lines.push(start_line(node));
            }
            continue;
        }
        stack.extend(named_children(node));
    }
    lines.sort_unstable();
    lines.dedup();
    lines
}

// Repeat imports of one source collapse into a single entry; a wildcard wins over named imports.
fn add_import(imports: &mut Vec<Import>, source: &str, names: Option<ImportNames>) {
    let Some(existing) = imports.iter_mut().find(|i| i.source == source) else {
        imports.push(Import {
            source: source.to_string(),
            names,
        });
        return;
    };
    match (&mut existing.names, names) {
        (Some(ImportNames::List(current)), Some(ImportNames::List(new))) => {
            for name in new {
                if !current.contains(&name) {
                    current.push(name);
                }
            }
        }
        (Some(ImportNames::Star(_)), _) | (_, None) => {}
        (slot, names) => *slot = names,
    }
}

/// Documents and config. They rank below code, and never count as an entry point.
pub fn is_data_file(file: &str) -> bool {
    [
        ".md",
        ".mdx",
        ".markdown",
        ".json",
        ".jsonc",
        ".yaml",
        ".yml",
    ]
    .iter()
    .any(|extension| file.ends_with(extension))
}

#[cfg(test)]
mod tests {
    use super::{extract, reference_lines};
    use crate::model::Symbol;

    fn find<'a>(symbols: &'a [Symbol], name: &str) -> &'a Symbol {
        symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("missing {name}"))
    }

    #[test]
    fn typescript_signatures_stop_before_the_body() {
        let source = [
            "/** Signs a user in. */",
            "export async function login(",
            "  name: string,",
            "): Promise<User> {",
            "  return x;",
            "}",
            "export const add = (a: number): number => a + 1;",
            "export const LIMIT: number = 5;",
            "export class Svc extends Base {",
            "  run(a: A): B { return b; }",
            "}",
            "export type Id = string | number;",
            "",
        ]
        .join("\n");
        let r = extract("ts", &source).unwrap();
        let login = find(&r.symbols, "login");
        assert_eq!(
            login.signature.as_deref(),
            Some("async function login( name: string, ): Promise<User>")
        );
        assert_eq!(login.doc.as_deref(), Some("Signs a user in."));
        for (name, expected) in [
            ("add", "const add = (a: number): number"),
            ("LIMIT", "const LIMIT: number"),
            ("Svc", "class Svc extends Base"),
            ("run", "run(a: A): B"),
            ("Id", "type Id = string | number"),
        ] {
            assert_eq!(find(&r.symbols, name).signature.as_deref(), Some(expected));
        }
    }

    #[test]
    fn python_signatures_and_docstrings() {
        let source = [
            "# about plain",
            "def plain(a):",
            "    return a",
            "",
            "@cached",
            "def helper(a: int) -> str:",
            "    \"\"\"Formats a number.",
            "",
            "    More detail.",
            "    \"\"\"",
            "    return str(a)",
            "",
        ]
        .join("\n");
        let r = extract("py", &source).unwrap();
        let helper = find(&r.symbols, "helper");
        assert_eq!(
            helper.signature.as_deref(),
            Some("def helper(a: int) -> str")
        );
        assert_eq!(helper.doc.as_deref(), Some("Formats a number."));
        assert_eq!(
            find(&r.symbols, "plain").doc.as_deref(),
            Some("about plain")
        );
    }

    #[test]
    fn rust_signatures_and_doc_comments() {
        let source = [
            "// plain comment",
            "pub fn undocumented() {}",
            "",
            "/// Adds two numbers.",
            "/// With overflow.",
            "#[inline]",
            "pub fn add(a: u8, b: u8) -> Result<u8, E> {",
            "    Ok(a + b)",
            "}",
            "pub const LIMIT: u32 = 1;",
            "",
        ]
        .join("\n");
        let r = extract("rs", &source).unwrap();
        let add = find(&r.symbols, "add");
        assert_eq!(
            add.signature.as_deref(),
            Some("pub fn add(a: u8, b: u8) -> Result<u8, E>")
        );
        assert_eq!(add.doc.as_deref(), Some("Adds two numbers."));
        assert_eq!(find(&r.symbols, "undocumented").doc, None);
        assert_eq!(
            find(&r.symbols, "LIMIT").signature.as_deref(),
            Some("pub const LIMIT: u32")
        );
    }

    #[test]
    fn references_skip_comments_strings_and_imports() {
        let source = [
            "import { load } from './a';",
            "// load in a comment",
            "const s = 'load in a string';",
            "const v = load();",
            "obj.load();",
            "const loader = 1;",
            "const { load: renamed } = api;",
            "const { load } = api;",
            "",
        ]
        .join("\n");
        assert_eq!(reference_lines("ts", &source, "load"), vec![4, 5, 7, 8]);
    }

    #[test]
    fn references_in_python_and_rust() {
        let python = "from a import go\n# go\ngo()\nx.go()\n";
        assert_eq!(reference_lines("py", python, "go"), vec![3, 4]);
        let rust = "use a::go;\n// go\nfn f() {\n    go();\n    s.go();\n    vec![go()];\n}\n";
        assert_eq!(reference_lines("rs", rust, "go"), vec![4, 5, 6]);
    }
}
