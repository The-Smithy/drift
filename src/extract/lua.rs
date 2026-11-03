use tree_sitter::Node;

use super::{
    Extracted, add_import, header_text, line_count, named_children, parse, start_line, symbol, text,
};
use crate::model::{Import, Symbol, TestCase, TestFile, TestSuite};

pub fn extract(source: &str) -> Option<Extracted> {
    let tree = parse(source, tree_sitter_lua::LANGUAGE.into())?;
    let root = tree.root_node();
    let mut visitor = Visitor {
        source,
        symbols: Vec::new(),
    };
    visitor.visit_block(root);
    let mut symbols = visitor.symbols;
    symbols.sort_by_key(|s| s.line);
    let tests = build_tests(root, &symbols, source);
    Some(Extracted {
        lang: "lua",
        lines: line_count(source),
        symbols,
        imports: collect_imports(root, source),
        tests,
    })
}

struct Visitor<'a> {
    source: &'a str,
    symbols: Vec<Symbol>,
}

// `a.b.c` and `a:b` become the parent `b`-style last table name and the field.
fn split_name<'a>(node: Node, source: &'a str) -> Option<(Option<&'a str>, &'a str)> {
    match node.kind() {
        "identifier" => Some((None, text(node, source))),
        "dot_index_expression" | "method_index_expression" => {
            let field = node
                .child_by_field_name("field")
                .or_else(|| node.child_by_field_name("method"))?;
            let table = text(node.child_by_field_name("table")?, source);
            let parent = table.rsplit(['.', ':']).next().unwrap_or(table);
            Some((Some(parent), text(field, source)))
        }
        _ => None,
    }
}

fn is_constant_name(name: &str) -> bool {
    name.len() >= 2
        && name.starts_with(|c: char| c.is_ascii_uppercase())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

impl Visitor<'_> {
    fn visit_block(&mut self, node: Node) {
        for child in named_children(node) {
            match child.kind() {
                "function_declaration" => self.function(child),
                "variable_declaration" => {
                    for inner in named_children(child) {
                        if inner.kind() == "assignment_statement" {
                            self.assignment(inner, child, true);
                        }
                    }
                }
                "assignment_statement" => self.assignment(child, child, false),
                "return_statement" => {
                    for list in named_children(child) {
                        for value in named_children(list) {
                            if value.kind() == "table_constructor" {
                                self.table_fields(value, None);
                            }
                        }
                    }
                }
                "do_statement" | "if_statement" | "else_statement" | "elseif_statement"
                | "block" | "ERROR" => self.visit_block(child),
                _ => {}
            }
        }
    }

    fn function(&mut self, node: Node) {
        let source = self.source;
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let Some((parent, name)) = split_name(name_node, source) else {
            return;
        };
        let exported = !text(node, source).starts_with("local");
        let entry = symbol(source, name, "function", node, exported, false, parent);
        self.symbols.push(entry);
    }

    fn assignment(&mut self, node: Node, outer: Node, is_local: bool) {
        let source = self.source;
        let mut names = Vec::new();
        let mut values = Vec::new();
        for child in named_children(node) {
            match child.kind() {
                "variable_list" => names = named_children(child),
                "expression_list" => values = named_children(child),
                _ => {}
            }
        }
        for (at, target) in names.iter().enumerate() {
            let Some((parent, name)) = split_name(*target, source) else {
                continue;
            };
            let value = values.get(at);
            let is_function = value.is_some_and(|v| v.kind() == "function_definition");
            let is_table = value.is_some_and(|v| v.kind() == "table_constructor");
            // `local util = require("util")` and other plain locals aren't definitions.
            if is_local && !is_function && !is_table && !is_constant_name(name) {
                continue;
            }
            let exported = !is_local;
            let kind = if is_function { "function" } else { "const" };
            let mut entry = symbol(source, name, kind, outer, exported, false, parent);
            let end = match value {
                Some(v) if is_function => v
                    .child_by_field_name("body")
                    .map_or(v.start_byte(), |body| body.start_byte()),
                Some(v) => v.start_byte(),
                None => outer.end_byte(),
            };
            entry.signature = header_text(&source[outer.start_byte()..end]);
            self.symbols.push(entry);
            if is_table && let Some(table) = value {
                self.table_fields(*table, Some(name));
            }
        }
    }

    // `local M = { run = function() end }` defines M.run.
    fn table_fields(&mut self, table: Node, parent: Option<&str>) {
        let source = self.source;
        for field in named_children(table) {
            if field.kind() != "field" {
                continue;
            }
            let (Some(name), Some(value)) = (
                field.child_by_field_name("name"),
                field.child_by_field_name("value"),
            ) else {
                continue;
            };
            if name.kind() != "identifier" || value.kind() != "function_definition" {
                continue;
            }
            let mut entry = symbol(
                source,
                text(name, source),
                "function",
                field,
                true,
                false,
                parent,
            );
            let end = value
                .child_by_field_name("body")
                .map_or(value.start_byte(), |body| body.start_byte());
            entry.signature = header_text(&source[field.start_byte()..end]);
            self.symbols.push(entry);
        }
    }
}

fn string_value<'a>(node: Node, source: &'a str) -> Option<&'a str> {
    if node.kind() == "string" {
        return node
            .child_by_field_name("content")
            .map(|content| text(content, source));
    }
    named_children(node)
        .into_iter()
        .find_map(|child| string_value(child, source))
}

fn collect_imports(root: Node, source: &str) -> Vec<Import> {
    let mut imports = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "function_call"
            && node
                .child_by_field_name("name")
                .is_some_and(|name| text(name, source) == "require")
            && let Some(module) = node
                .child_by_field_name("arguments")
                .and_then(|arguments| string_value(arguments, source))
        {
            add_import(&mut imports, module, None);
            continue;
        }
        let mut children = named_children(node);
        children.reverse();
        stack.extend(children);
    }
    imports
}

fn build_tests(root: Node, symbols: &[Symbol], source: &str) -> Option<TestFile> {
    let mut suites = Vec::new();
    let mut loose = Vec::new();
    busted_calls(root, source, None, &mut suites, &mut loose);
    if !suites.is_empty() || !loose.is_empty() {
        if let Some(first) = loose.first().map(|case: &TestCase| case.line) {
            suites.push(TestSuite {
                name: "(module)".to_string(),
                line: first,
                cases: loose,
            });
        }
        return Some(TestFile {
            framework: "busted".to_string(),
            suites,
        });
    }
    if !source.contains("luaunit") {
        return None;
    }

    // LuaUnit runs `testX` globals and methods of `TestX` tables.
    let mut by_table: Vec<TestSuite> = Vec::new();
    for function in symbols
        .iter()
        .filter(|s| s.kind == "function" && s.name.to_lowercase().starts_with("test"))
    {
        let suite_name = match function.parent.as_deref() {
            Some(parent) if parent.to_lowercase().starts_with("test") => parent,
            Some(_) => continue,
            None => "(module)",
        };
        let case = TestCase {
            name: function.name.clone(),
            line: function.line,
        };
        match by_table.iter_mut().find(|suite| suite.name == suite_name) {
            Some(suite) => suite.cases.push(case),
            None => by_table.push(TestSuite {
                name: suite_name.to_string(),
                line: function.line,
                cases: vec![case],
            }),
        }
    }
    (!by_table.is_empty()).then(|| TestFile {
        framework: "luaunit".to_string(),
        suites: by_table,
    })
}

fn busted_calls(
    node: Node,
    source: &str,
    suite: Option<usize>,
    suites: &mut Vec<TestSuite>,
    loose: &mut Vec<TestCase>,
) {
    for child in named_children(node) {
        let mut inner_suite = suite;
        if child.kind() == "function_call"
            && let Some(name) = child.child_by_field_name("name").map(|n| text(n, source))
            && matches!(name, "describe" | "context" | "it")
            && let Some(title) = child
                .child_by_field_name("arguments")
                .and_then(|arguments| string_value(arguments, source))
        {
            if name == "it" {
                let case = TestCase {
                    name: title.to_string(),
                    line: start_line(child),
                };
                match suite {
                    Some(at) => suites[at].cases.push(case),
                    None => loose.push(case),
                }
            } else {
                suites.push(TestSuite {
                    name: title.to_string(),
                    line: start_line(child),
                    cases: Vec::new(),
                });
                inner_suite = Some(suites.len() - 1);
            }
        }
        busted_calls(child, source, inner_suite, suites, loose);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(source: &str) -> Vec<(String, String, bool, Option<String>)> {
        extract(source)
            .unwrap()
            .symbols
            .into_iter()
            .map(|s| (s.name, s.kind, s.exported, s.parent))
            .collect()
    }

    fn row(
        name: &str,
        kind: &str,
        exported: bool,
        parent: Option<&str>,
    ) -> (String, String, bool, Option<String>) {
        (
            name.to_string(),
            kind.to_string(),
            exported,
            parent.map(str::to_string),
        )
    }

    #[test]
    fn finds_every_way_to_define_a_function() {
        let source = "function add(a) end\nlocal function helper(x) end\nlocal M = {}\nfunction M.run(x) end\nfunction M:method(y) end\nM.other = function(z) end\nlocal anon = function() end\nfunction a.b.deep() end\nreturn M\n";
        assert_eq!(
            names(source),
            vec![
                row("add", "function", true, None),
                row("helper", "function", false, None),
                row("M", "const", false, None),
                row("run", "function", true, Some("M")),
                row("method", "function", true, Some("M")),
                row("other", "function", true, Some("M")),
                row("anon", "function", false, None),
                row("deep", "function", true, Some("b")),
            ]
        );
    }

    #[test]
    fn plain_locals_are_not_definitions() {
        let source = "local util = require('util')\nlocal x = 1\nlocal MAX = 10\nVERSION = '1'\nlocal function f()\n  local inner = 1\n  local function nested() end\nend\n";
        assert_eq!(
            names(source),
            vec![
                row("MAX", "const", false, None),
                row("VERSION", "const", true, None),
                row("f", "function", false, None),
            ]
        );
    }

    #[test]
    fn functions_in_tables_count() {
        let source = "local M = {\n  run = function(a) end,\n  value = 1,\n}\nreturn {\n  setup = function() end,\n}\n";
        let found = names(source);
        assert!(
            found.contains(&row("run", "function", true, Some("M"))),
            "{found:?}"
        );
        assert!(
            found.contains(&row("setup", "function", true, None)),
            "{found:?}"
        );
        assert!(!found.iter().any(|f| f.0 == "value"), "{found:?}");
    }

    #[test]
    fn signature_and_doc() {
        let source = "--- Adds numbers.\n-- More detail.\nfunction M.add(a, b)\n  return a + b\nend\nM.sub = function(a, b) return a - b end\n";
        let symbols = extract(source).unwrap().symbols;
        assert_eq!(
            symbols[0].signature.as_deref(),
            Some("function M.add(a, b)")
        );
        assert_eq!(symbols[0].doc.as_deref(), Some("Adds numbers."));
        assert_eq!(
            symbols[1].signature.as_deref(),
            Some("M.sub = function(a, b)")
        );
        assert_eq!((symbols[0].line, symbols[0].end_line), (3, Some(5)));
    }

    #[test]
    fn finds_requires_in_every_form() {
        let source = "local a = require('a.b')\nlocal c = require \"c\"\nlocal function f() return require('lazy') end\nrequire('a.b')\n";
        let imports: Vec<String> = extract(source)
            .unwrap()
            .imports
            .into_iter()
            .map(|i| i.source)
            .collect();
        assert_eq!(imports, vec!["a.b", "c", "lazy"]);
    }

    #[test]
    fn finds_busted_tests() {
        let source = "describe('thing', function()\n  it('works', function() end)\n  describe('inner', function()\n    it('nests', function() end)\n  end)\nend)\nit('loose', function() end)\n";
        let tests = extract(source).unwrap().tests.unwrap();
        assert_eq!(tests.framework, "busted");
        let suites: Vec<(&str, usize)> = tests
            .suites
            .iter()
            .map(|s| (s.name.as_str(), s.cases.len()))
            .collect();
        assert_eq!(suites, vec![("thing", 1), ("inner", 1), ("(module)", 1)]);
    }

    #[test]
    fn finds_luaunit_tests() {
        let source = "local lu = require('luaunit')\nTestMath = {}\nfunction TestMath:testAdd() end\nfunction testLoose() end\nfunction helper() end\n";
        let tests = extract(source).unwrap().tests.unwrap();
        assert_eq!(tests.framework, "luaunit");
        assert_eq!(tests.suites.len(), 2);
    }

    #[test]
    fn ordinary_files_have_no_tests() {
        assert!(extract("function test() end\n").unwrap().tests.is_none());
    }
}
