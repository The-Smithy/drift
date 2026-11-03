use std::collections::HashSet;

use tree_sitter::Node;

use super::{
    Extracted, add_import, has_child_kind, line_count, named_children, parse, symbol, text,
};
use crate::model::{Import, ImportNames, Symbol, TestCase, TestFile, TestSuite};

#[derive(Clone, Copy)]
enum Scope<'a> {
    Module,
    Class(&'a str),
    Function(&'a str),
}

impl<'a> Scope<'a> {
    fn name(self) -> Option<&'a str> {
        match self {
            Scope::Module => None,
            Scope::Class(name) | Scope::Function(name) => Some(name),
        }
    }
}

pub fn extract(source: &str) -> Option<Extracted> {
    let tree = parse(source, tree_sitter_python::LANGUAGE.into())?;
    let root = tree.root_node();

    let mut visitor = Visitor {
        source,
        symbols: Vec::new(),
        dunder_all: None,
        class_bases: Vec::new(),
    };
    visitor.visit_block(root, Scope::Module);

    if let Some(public) = &visitor.dunder_all {
        for symbol in visitor.symbols.iter_mut().filter(|s| s.parent.is_none()) {
            symbol.exported = public.contains(&symbol.name);
        }
    }

    let mut symbols = visitor.symbols;
    symbols.sort_by_key(|s| s.line);
    let tests = build_tests(&symbols, &test_case_classes(&visitor.class_bases), source);

    Some(Extracted {
        lang: "python",
        lines: line_count(source),
        symbols,
        imports: collect_imports(root, source),
        tests,
    })
}

struct Visitor<'a> {
    source: &'a str,
    symbols: Vec<Symbol>,
    dunder_all: Option<HashSet<String>>,
    class_bases: Vec<(&'a str, &'a str)>,
}

impl<'a> Visitor<'a> {
    fn visit_block(&mut self, node: Node, scope: Scope) {
        for child in named_children(node) {
            self.visit(child, scope);
        }
    }

    fn visit(&mut self, node: Node, scope: Scope) {
        match node.kind() {
            "decorated_definition" => {
                let decorators: Vec<String> = named_children(node)
                    .into_iter()
                    .filter(|c| c.kind() == "decorator")
                    .filter_map(|d| decorator_name(d, self.source))
                    .collect();
                if let Some(definition) = node.child_by_field_name("definition") {
                    self.visit_definition(definition, scope, decorators);
                }
            }
            "function_definition" | "class_definition" => {
                self.visit_definition(node, scope, Vec::new())
            }
            "expression_statement" if matches!(scope, Scope::Module) => {
                for child in named_children(node) {
                    if child.kind() == "assignment" {
                        self.visit_assignment(child);
                    }
                }
            }
            "type_alias_statement" if matches!(scope, Scope::Module) => {
                if let Some(left) = node.child_by_field_name("left") {
                    let name = text(left, self.source);
                    let exported = !name.starts_with('_');
                    self.symbols.push(symbol(
                        self.source,
                        name,
                        "type",
                        node,
                        exported,
                        false,
                        None,
                    ));
                }
            }
            "block"
            | "if_statement"
            | "elif_clause"
            | "else_clause"
            | "try_statement"
            | "except_clause"
            | "except_group_clause"
            | "finally_clause"
            | "with_statement"
            | "for_statement"
            | "while_statement"
            | "ERROR" => self.visit_block(node, scope),
            _ => {}
        }
    }

    fn visit_definition(&mut self, node: Node, scope: Scope, decorators: Vec<String>) {
        let source = self.source;
        let Some(name) = node.child_by_field_name("name").map(|n| text(n, source)) else {
            return;
        };
        let exported = !name.starts_with('_') && !matches!(scope, Scope::Function(_));
        let is_class = node.kind() == "class_definition";
        let kind = if is_class { "class" } else { "function" };
        let is_async = !is_class && has_child_kind(node, "async");

        let mut entry = symbol(
            self.source,
            name,
            kind,
            node,
            exported,
            is_async,
            scope.name(),
        );
        entry.decorators = Some(decorators);
        self.symbols.push(entry);

        if is_class && let Some(bases) = node.child_by_field_name("superclasses") {
            self.class_bases.push((name, text(bases, source)));
        }

        if let Some(body) = node.child_by_field_name("body") {
            let inner = if is_class {
                Scope::Class(name)
            } else {
                Scope::Function(name)
            };
            self.visit_block(body, inner);
        }
    }

    fn visit_assignment(&mut self, node: Node) {
        let Some(left) = node.child_by_field_name("left") else {
            return;
        };
        if left.kind() != "identifier" {
            return;
        }
        let name = text(left, self.source);

        if name == "__all__" {
            if let Some(right) = node.child_by_field_name("right")
                && matches!(right.kind(), "list" | "tuple")
            {
                self.dunder_all = Some(
                    named_children(right)
                        .into_iter()
                        .filter(|item| item.kind() == "string")
                        .map(|item| {
                            text(item, self.source)
                                .trim_matches(|c| c == '"' || c == '\'')
                                .to_string()
                        })
                        .collect(),
                );
            }
            return;
        }

        if is_constant_name(name) {
            self.symbols
                .push(symbol(self.source, name, "const", node, true, false, None));
        }
    }
}

// Follows inheritance within the file, so `class A(Base)` counts when `class Base(TestCase)` does.
fn test_case_classes(class_bases: &[(&str, &str)]) -> HashSet<String> {
    let mut found: HashSet<String> = HashSet::new();
    loop {
        let before = found.len();
        for (name, bases) in class_bases {
            let inherits = bases
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .any(|base| base.ends_with("TestCase") || found.contains(base));
            if inherits {
                found.insert(name.to_string());
            }
        }
        if found.len() == before {
            return found;
        }
    }
}

fn is_constant_name(name: &str) -> bool {
    name.len() >= 2
        && name.starts_with(|c: char| c.is_ascii_uppercase())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn decorator_name(decorator: Node, source: &str) -> Option<String> {
    let raw = text(decorator, source).trim_start_matches('@').trim();
    let name = raw.split('(').next()?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

// Walks the whole tree because lazy imports inside functions are still real dependencies.
fn collect_imports(root: Node, source: &str) -> Vec<Import> {
    let mut imports = Vec::new();
    let mut stack = vec![root];

    while let Some(node) = stack.pop() {
        match node.kind() {
            "import_statement" => {
                for name in field_nodes(node, "name") {
                    add_import(&mut imports, imported_name(name, source), None);
                }
            }
            "import_from_statement" => {
                let Some(module) = node.child_by_field_name("module_name") else {
                    continue;
                };
                let names = if has_named_kind(node, "wildcard_import") {
                    ImportNames::star()
                } else {
                    ImportNames::List(
                        field_nodes(node, "name")
                            .into_iter()
                            .map(|n| imported_name(n, source).to_string())
                            .collect(),
                    )
                };
                add_import(&mut imports, text(module, source), Some(names));
            }
            _ => {
                let mut children = named_children(node);
                children.reverse();
                stack.extend(children);
            }
        }
    }
    imports
}

fn field_nodes<'t>(node: Node<'t>, field: &str) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.children_by_field_name(field, &mut cursor).collect()
}

fn has_named_kind(node: Node, kind: &str) -> bool {
    named_children(node).iter().any(|c| c.kind() == kind)
}

fn imported_name<'a>(node: Node, source: &'a str) -> &'a str {
    let target = if node.kind() == "aliased_import" {
        node.child_by_field_name("name").unwrap_or(node)
    } else {
        node
    };
    text(target, source)
}

fn build_tests(
    symbols: &[Symbol],
    test_case_classes: &HashSet<String>,
    source: &str,
) -> Option<TestFile> {
    let mut suites: Vec<TestSuite> = symbols
        .iter()
        .filter(|s| {
            s.kind == "class"
                && s.parent.is_none()
                && (s.name.starts_with("Test") || test_case_classes.contains(&s.name))
        })
        .map(|s| TestSuite {
            name: s.name.clone(),
            line: s.line,
            cases: Vec::new(),
        })
        .collect();

    let mut module_cases = Vec::new();
    for function in symbols
        .iter()
        .filter(|s| s.kind == "function" && s.name.starts_with("test_"))
    {
        let case = TestCase {
            name: function.name.clone(),
            line: function.line,
        };
        match &function.parent {
            None => module_cases.push(case),
            Some(parent) => {
                if let Some(suite) = suites.iter_mut().find(|s| &s.name == parent) {
                    suite.cases.push(case);
                }
            }
        }
    }

    suites.retain(|suite| !suite.cases.is_empty());
    if let Some(first) = module_cases.first() {
        suites.push(TestSuite {
            name: "(module)".to_string(),
            line: first.line,
            cases: module_cases,
        });
    }
    if suites.is_empty() {
        return None;
    }

    let framework = if source.lines().take(20).any(|l| l.contains("unittest")) {
        "unittest"
    } else {
        "pytest"
    };
    Some(TestFile {
        framework: framework.to_string(),
        suites,
    })
}

#[cfg(test)]
mod tests {
    use crate::extract::{Extracted, extract};
    use crate::model::{ImportNames, Symbol};

    fn py(source: &str) -> Extracted {
        extract("py", source).unwrap()
    }

    fn find<'a>(result: &'a Extracted, name: &str) -> &'a Symbol {
        result
            .symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("missing symbol {name}"))
    }

    #[test]
    fn functions_classes_and_constants() {
        let source = [
            "MAX_SIZE = 10",
            "lower_case = 1",
            "TIMEOUT: int = 5",
            "",
            "class Foo:",
            "    LIMIT = 1",
            "    def bar(self):",
            "        return 1",
            "    async def _baz(self):",
            "        pass",
            "",
            "def top():",
            "    def nested():",
            "        pass",
            "",
        ]
        .join("\n");
        let r = py(&source);
        assert_eq!(find(&r, "MAX_SIZE").kind, "const");
        assert_eq!(find(&r, "TIMEOUT").kind, "const");
        assert!(
            !r.symbols
                .iter()
                .any(|s| s.name == "lower_case" || s.name == "LIMIT")
        );

        let bar = find(&r, "bar");
        assert_eq!(bar.parent.as_deref(), Some("Foo"));
        assert!(bar.exported);
        assert_eq!((bar.line, bar.end_line), (7, Some(8)));

        let baz = find(&r, "_baz");
        assert!(baz.is_async && !baz.exported);

        let nested = find(&r, "nested");
        assert_eq!(nested.parent.as_deref(), Some("top"));
        assert!(!nested.exported);
    }

    #[test]
    fn decorators_without_arguments() {
        let r = py("@app.route('/x')\n@cached\ndef handler():\n    pass\n");
        let handler = find(&r, "handler");
        assert_eq!(handler.line, 3);
        assert_eq!(
            handler.decorators.as_deref(),
            Some(&["app.route".to_string(), "cached".to_string()][..])
        );
    }

    #[test]
    fn multiline_relative_and_lazy_imports() {
        let source = [
            "from foo import (",
            "    a,",
            "    b as c,",
            ")",
            "from . import sibling",
            "from ..pkg.mod import *",
            "import os, sys as system",
            "def f():",
            "    import json",
            "",
        ]
        .join("\n");
        let r = py(&source);
        let names = |source: &str| {
            r.imports
                .iter()
                .find(|i| i.source == source)
                .unwrap_or_else(|| panic!("missing import {source}"))
                .names
                .clone()
        };
        assert_eq!(
            names("foo"),
            Some(ImportNames::List(vec!["a".to_string(), "b".to_string()]))
        );
        assert_eq!(
            names("."),
            Some(ImportNames::List(vec!["sibling".to_string()]))
        );
        assert_eq!(names("..pkg.mod"), Some(ImportNames::star()));
        assert_eq!(names("os"), None);
        assert_eq!(names("sys"), None);
        assert_eq!(names("json"), None);
    }

    #[test]
    fn definitions_inside_module_level_conditionals() {
        let source = [
            "try:",
            "    def a():",
            "        pass",
            "except ImportError:",
            "    def b():",
            "        pass",
            "if True:",
            "    class C:",
            "        pass",
            "",
        ]
        .join("\n");
        let r = py(&source);
        for name in ["a", "b", "C"] {
            find(&r, name);
        }
    }

    #[test]
    fn dunder_all_controls_exports() {
        let r = py("__all__ = ['keep']\ndef keep():\n    pass\ndef drop():\n    pass\n");
        assert!(find(&r, "keep").exported);
        assert!(!find(&r, "drop").exported);
    }

    #[test]
    fn tests_group_by_class_and_module() {
        let source = [
            "import pytest",
            "class TestThing:",
            "    def test_a(self):",
            "        pass",
            "    async def test_b(self):",
            "        pass",
            "def test_c():",
            "    pass",
            "def test_d():",
            "    pass",
            "",
        ]
        .join("\n");
        let tests = py(&source).tests.unwrap();
        assert_eq!(tests.framework, "pytest");
        assert_eq!(tests.suites.len(), 2);
        assert_eq!(tests.suites[0].name, "TestThing");
        assert_eq!(tests.suites[0].cases.len(), 2);
        assert_eq!(tests.suites[1].name, "(module)");
        assert_eq!(tests.suites[1].cases.len(), 2);
    }

    #[test]
    fn test_classes_inherit_through_a_local_base() {
        let source = [
            "import unittest",
            "class Sandbox(unittest.TestCase):",
            "    def setUp(self):",
            "        pass",
            "class Draft(Sandbox):",
            "    def test_a(self):",
            "        pass",
            "class Helper:",
            "    def test_connection(self):",
            "        pass",
            "class TestData:",
            "    value = 1",
            "",
        ]
        .join("\n");
        let tests = py(&source).tests.unwrap();
        let names: Vec<_> = tests.suites.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Draft"]);
        assert_eq!(tests.suites[0].cases[0].name, "test_a");
    }

    #[test]
    fn unittest_classes_without_test_prefix() {
        let source = [
            "import unittest",
            "class ThingTests(unittest.TestCase):",
            "    def test_a(self):",
            "        pass",
            "",
        ]
        .join("\n");
        let tests = py(&source).tests.unwrap();
        assert_eq!(tests.framework, "unittest");
        assert_eq!(tests.suites[0].name, "ThingTests");
        assert_eq!(tests.suites[0].cases.len(), 1);
    }
}
