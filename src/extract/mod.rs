mod python;
mod rust;
mod sql;
mod typescript;

use tree_sitter::{Language, Node, Parser, Tree};

use crate::model::{Import, ImportNames, Symbol, TestFile};

pub const UNKNOWN_FRAMEWORK: &str = "unknown";

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
        "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" | "py" | "rs" | "sql"
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
        _ => None,
    }
}

fn parse(source: &str, language: Language) -> Option<Tree> {
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
    }
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
