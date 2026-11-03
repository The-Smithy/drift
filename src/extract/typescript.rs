use std::collections::HashSet;

use tree_sitter::{Language, Node};

use super::{
    Extracted, UNKNOWN_FRAMEWORK, add_import, has_child_kind, line_count, named_children, parse,
    start_line, symbol, text,
};
use crate::model::{Import, ImportNames, Symbol, TestCase, TestFile, TestSuite};

const TEST_MODIFIERS: &[&str] = &[
    "only",
    "skip",
    "each",
    "concurrent",
    "todo",
    "failing",
    "sequential",
    "skipIf",
    "runIf",
    "fixme",
    "slow",
    "parallel",
    "serial",
];

pub fn extract(source: &str, language: Language, lang: &'static str) -> Option<Extracted> {
    let tree = parse(source, language)?;
    let root = tree.root_node();
    let mut visitor = Visitor {
        source,
        symbols: Vec::new(),
        imports: Vec::new(),
        exported_names: HashSet::new(),
        public_methods: Vec::new(),
    };
    visitor.visit_statements(root, None);
    visitor.apply_late_exports();

    let mut symbols = visitor.symbols;
    let mut imports = visitor.imports;
    symbols.sort_by_key(|s| s.line);

    let suites = scan_calls(root, source, &mut imports);
    let tests = (!suites.is_empty()).then(|| TestFile {
        framework: detect_framework(source).to_string(),
        suites,
    });

    Some(Extracted {
        lang,
        lines: line_count(source),
        symbols,
        imports,
        tests,
    })
}

struct Visitor<'a> {
    source: &'a str,
    symbols: Vec<Symbol>,
    imports: Vec<Import>,
    exported_names: HashSet<String>,
    public_methods: Vec<(usize, String)>,
}

impl<'a> Visitor<'a> {
    fn visit_statements(&mut self, node: Node, parent: Option<&str>) {
        for child in named_children(node) {
            self.visit(child, false, parent);
        }
    }

    fn visit(&mut self, node: Node, exported: bool, parent: Option<&str>) {
        match node.kind() {
            "export_statement" => self.visit_export(node, parent),
            "import_statement" => self.visit_import(node),
            "expression_statement" => {
                for child in named_children(node) {
                    match child.kind() {
                        "internal_module" | "module" => self.visit(child, exported, parent),
                        "assignment_expression" => self.visit_commonjs_export(child),
                        _ => self.visit_iife(child, parent),
                    }
                }
            }
            "ambient_declaration" | "ERROR" => {
                for child in named_children(node) {
                    self.visit(child, exported, parent);
                }
            }
            "function_declaration" | "generator_function_declaration" | "function_signature" => {
                if let Some(name) = self.field_text(node, "name") {
                    let is_async = has_child_kind(node, "async");
                    self.symbols.push(symbol(
                        self.source,
                        name,
                        "function",
                        node,
                        exported,
                        is_async,
                        parent,
                    ));
                }
            }
            "class_declaration" | "abstract_class_declaration" => {
                self.visit_class(node, exported, parent)
            }
            "interface_declaration" => self.push_named(node, "interface", exported, parent),
            "type_alias_declaration" => self.push_named(node, "type", exported, parent),
            "enum_declaration" => self.push_named(node, "enum", exported, parent),
            "internal_module" | "module" => {
                let Some(name) = self.field_text(node, "name") else {
                    return;
                };
                let name = name.trim_matches(|c| c == '"' || c == '\'');
                self.symbols.push(symbol(
                    self.source,
                    name,
                    "module",
                    node,
                    exported,
                    false,
                    parent,
                ));
                if let Some(body) = node.child_by_field_name("body") {
                    self.visit_statements(body, Some(name));
                }
            }
            "lexical_declaration" | "variable_declaration" => {
                for declarator in named_children(node) {
                    if declarator.kind() == "variable_declarator" {
                        self.visit_declarator(declarator, node, exported, parent);
                    }
                }
            }
            _ => {}
        }
    }

    // Legacy browser and UMD code keeps its real top level inside a wrapper function.
    fn visit_iife(&mut self, expression: Node, parent: Option<&str>) {
        let call = unwrap_expression(expression);
        if call.kind() != "call_expression" {
            return;
        }
        let Some(mut callee) = call.child_by_field_name("function").map(unwrap_expression) else {
            return;
        };
        if callee.kind() == "member_expression"
            && let Some(object) = callee.child_by_field_name("object")
        {
            callee = unwrap_expression(object);
        }
        if !is_function_value(callee) {
            return;
        }

        let arguments = call
            .child_by_field_name("arguments")
            .map(named_children)
            .unwrap_or_default();
        let wrappers = std::iter::once(callee).chain(
            arguments
                .into_iter()
                .filter(|argument| is_function_value(*argument)),
        );
        for wrapper in wrappers {
            if let Some(body) = wrapper.child_by_field_name("body")
                && body.kind() == "statement_block"
            {
                self.visit_statements(body, parent);
            }
        }
    }

    fn push_named(&mut self, node: Node, kind: &str, exported: bool, parent: Option<&str>) {
        if let Some(name) = self.field_text(node, "name") {
            self.symbols.push(symbol(
                self.source,
                name,
                kind,
                node,
                exported,
                false,
                parent,
            ));
        }
    }

    fn field_text(&self, node: Node, field: &str) -> Option<&'a str> {
        node.child_by_field_name(field)
            .map(|n| text(n, self.source))
    }

    fn visit_export(&mut self, node: Node, parent: Option<&str>) {
        if let Some(source_node) = node.child_by_field_name("source") {
            let Some(source) = string_value(source_node, self.source) else {
                return;
            };
            let names = if has_child_kind(node, "*") || has_child_kind(node, "namespace_export") {
                ImportNames::star()
            } else {
                ImportNames::List(self.export_clause_names(node))
            };
            add_import(&mut self.imports, source, Some(names));
            return;
        }
        if let Some(declaration) = node.child_by_field_name("declaration") {
            self.visit(declaration, true, parent);
            return;
        }
        if let Some(value) = node.child_by_field_name("value") {
            if value.kind() == "identifier" {
                self.exported_names
                    .insert(text(value, self.source).to_string());
            }
            return;
        }
        for name in self.export_clause_names(node) {
            self.exported_names.insert(name);
        }
    }

    fn export_clause_names(&self, node: Node) -> Vec<String> {
        let mut names = Vec::new();
        for clause in named_children(node) {
            if clause.kind() != "export_clause" {
                continue;
            }
            for specifier in named_children(clause) {
                if let Some(name) = self.field_text(specifier, "name") {
                    names.push(name.to_string());
                }
            }
        }
        names
    }

    fn visit_import(&mut self, node: Node) {
        let mut source_node = node.child_by_field_name("source");
        let mut names: Vec<String> = Vec::new();
        let mut star = false;

        for child in named_children(node) {
            match child.kind() {
                "import_clause" => {
                    for part in named_children(child) {
                        match part.kind() {
                            "identifier" => names.push(text(part, self.source).to_string()),
                            "namespace_import" => star = true,
                            "named_imports" => {
                                for specifier in named_children(part) {
                                    if let Some(name) = self.field_text(specifier, "name") {
                                        names.push(name.to_string());
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                "import_require_clause" => {
                    source_node = child.child_by_field_name("source");
                    for part in named_children(child) {
                        if part.kind() == "identifier" {
                            names.push(text(part, self.source).to_string());
                        }
                    }
                }
                _ => {}
            }
        }

        let Some(source) = source_node.and_then(|n| string_value(n, self.source)) else {
            return;
        };
        let names = if star {
            Some(ImportNames::star())
        } else if names.is_empty() {
            None
        } else {
            Some(ImportNames::List(names))
        };
        add_import(&mut self.imports, source, names);
    }

    fn visit_class(&mut self, node: Node, exported: bool, parent: Option<&str>) {
        let Some(name) = self.field_text(node, "name") else {
            return;
        };
        self.symbols.push(symbol(
            self.source,
            name,
            "class",
            node,
            exported,
            false,
            parent,
        ));

        let Some(body) = node.child_by_field_name("body") else {
            return;
        };
        for member in named_children(body) {
            let (name_node, is_async) = match member.kind() {
                "method_definition" | "abstract_method_signature" => (
                    member.child_by_field_name("name"),
                    has_child_kind(member, "async"),
                ),
                "public_field_definition" | "field_definition" => {
                    let Some(value) = member.child_by_field_name("value") else {
                        continue;
                    };
                    if !is_function_value(value) {
                        continue;
                    }
                    let name_node = member
                        .child_by_field_name("name")
                        .or_else(|| member.child_by_field_name("property"));
                    (name_node, has_child_kind(value, "async"))
                }
                _ => continue,
            };
            let Some(member_name) = name_node.map(|n| text(n, self.source)) else {
                continue;
            };
            if member_name == "constructor" {
                continue;
            }
            let private = member_name.starts_with('#') || self.has_restricted_access(member);
            if !private {
                self.public_methods
                    .push((self.symbols.len(), name.to_string()));
            }
            self.symbols.push(symbol(
                self.source,
                member_name,
                "function",
                member,
                false,
                is_async,
                Some(name),
            ));
        }
    }

    fn has_restricted_access(&self, member: Node) -> bool {
        named_children(member)
            .iter()
            .any(|c| c.kind() == "accessibility_modifier" && text(*c, self.source) != "public")
    }

    fn visit_declarator(
        &mut self,
        declarator: Node,
        declaration: Node,
        exported: bool,
        parent: Option<&str>,
    ) {
        let Some(name_node) = declarator.child_by_field_name("name") else {
            return;
        };
        let value = declarator.child_by_field_name("value");

        if name_node.kind() != "identifier" {
            let mut names = Vec::new();
            collect_pattern_names(name_node, self.source, &mut names);
            for name in names {
                self.symbols.push(symbol(
                    self.source,
                    name,
                    "const",
                    declaration,
                    exported,
                    false,
                    parent,
                ));
            }
            return;
        }

        let name = text(name_node, self.source);
        let (kind, is_async) = match value {
            Some(v) if is_function_value(v) => ("function", has_child_kind(v, "async")),
            _ => ("const", false),
        };
        self.symbols.push(symbol(
            self.source,
            name,
            kind,
            declaration,
            exported,
            is_async,
            parent,
        ));
    }

    fn visit_commonjs_export(&mut self, assignment: Node) {
        let (Some(left), Some(right)) = (
            assignment.child_by_field_name("left"),
            assignment.child_by_field_name("right"),
        ) else {
            return;
        };
        let target = text(left, self.source);

        if target == "module.exports" {
            match right.kind() {
                "identifier" => {
                    self.exported_names
                        .insert(text(right, self.source).to_string());
                }
                "object" => {
                    for property in named_children(right) {
                        let name = match property.kind() {
                            "shorthand_property_identifier" => Some(text(property, self.source)),
                            "pair" => self.field_text(property, "key"),
                            _ => None,
                        };
                        if let Some(name) = name {
                            self.exported_names.insert(name.to_string());
                        }
                    }
                }
                _ => {}
            }
            return;
        }

        let Some(name) = target
            .strip_prefix("module.exports.")
            .or_else(|| target.strip_prefix("exports."))
        else {
            return;
        };
        if is_function_value(right) {
            self.symbols.push(symbol(
                self.source,
                name,
                "function",
                assignment,
                true,
                has_child_kind(right, "async"),
                None,
            ));
        } else if right.kind() == "identifier" {
            self.exported_names
                .insert(text(right, self.source).to_string());
        } else {
            self.symbols.push(symbol(
                self.source,
                name,
                "const",
                assignment,
                true,
                false,
                None,
            ));
        }
    }

    fn apply_late_exports(&mut self) {
        for symbol in &mut self.symbols {
            if symbol.parent.is_none() && self.exported_names.contains(&symbol.name) {
                symbol.exported = true;
            }
        }
        let exported_classes: HashSet<String> = self
            .symbols
            .iter()
            .filter(|s| s.kind == "class" && s.exported)
            .map(|s| s.name.clone())
            .collect();
        for (index, class) in &self.public_methods {
            if exported_classes.contains(class) {
                self.symbols[*index].exported = true;
            }
        }
    }
}

fn unwrap_expression(mut node: Node) -> Node {
    loop {
        let inner = match node.kind() {
            "parenthesized_expression" => named_children(node).into_iter().next(),
            "unary_expression" => node.child_by_field_name("argument"),
            _ => None,
        };
        match inner {
            Some(next) => node = next,
            None => return node,
        }
    }
}

fn is_function_value(node: Node) -> bool {
    matches!(
        node.kind(),
        "arrow_function" | "function_expression" | "function" | "generator_function"
    )
}

fn string_value<'a>(node: Node, source: &'a str) -> Option<&'a str> {
    if !matches!(node.kind(), "string" | "template_string") {
        return None;
    }
    let raw = text(node, source);
    (raw.len() >= 2).then(|| &raw[1..raw.len() - 1])
}

fn collect_pattern_names<'a>(node: Node, source: &'a str, out: &mut Vec<&'a str>) {
    match node.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => out.push(text(node, source)),
        "pair_pattern" => {
            if let Some(value) = node.child_by_field_name("value") {
                collect_pattern_names(value, source, out);
            }
        }
        "object_assignment_pattern" | "assignment_pattern" => {
            if let Some(left) = node.child_by_field_name("left") {
                collect_pattern_names(left, source, out);
            }
        }
        "object_pattern" | "array_pattern" | "rest_pattern" => {
            for child in named_children(node) {
                collect_pattern_names(child, source, out);
            }
        }
        _ => {}
    }
}

enum TestCall {
    Suite,
    Case { allow_no_body: bool },
}

fn classify_test_call(function: Node, source: &str) -> Option<TestCall> {
    let mut current = function;
    let mut properties = Vec::new();
    let root = loop {
        match current.kind() {
            "identifier" => break text(current, source),
            "member_expression" => {
                properties.push(text(current.child_by_field_name("property")?, source));
                current = current.child_by_field_name("object")?;
            }
            "call_expression" => current = current.child_by_field_name("function")?,
            _ => return None,
        }
    };

    let is_test_root = matches!(root, "it" | "test" | "specify" | "fit" | "xit" | "xtest");
    let is_suite_root = matches!(
        root,
        "describe" | "suite" | "context" | "fdescribe" | "xdescribe"
    );
    if !is_test_root && !is_suite_root {
        return None;
    }

    // Playwright spells suites as test.describe(...).
    let describes = is_test_root && properties.contains(&"describe");
    if !properties
        .iter()
        .all(|p| *p == "describe" || TEST_MODIFIERS.contains(p))
    {
        return None;
    }
    if is_suite_root || describes {
        return Some(TestCall::Suite);
    }
    Some(TestCall::Case {
        allow_no_body: properties.iter().any(|p| *p == "todo" || *p == "skip"),
    })
}

// Walks the whole tree with an explicit stack: minified or generated files can nest deeper than a thread stack.
fn scan_calls(root: Node, source: &str, imports: &mut Vec<Import>) -> Vec<TestSuite> {
    let mut suites: Vec<TestSuite> = Vec::new();
    let mut root_suite: Option<usize> = None;
    let mut stack: Vec<(Node, Option<usize>)> = vec![(root, None)];

    while let Some((node, enclosing)) = stack.pop() {
        let mut child_suite = enclosing;

        if node.kind() == "call_expression"
            && let Some(function) = node.child_by_field_name("function")
        {
            let arguments: Vec<_> = node
                .child_by_field_name("arguments")
                .filter(|a| a.kind() == "arguments")
                .map(named_children)
                .unwrap_or_default()
                .into_iter()
                .filter(|a| a.kind() != "comment")
                .collect();
            let first_string = arguments.first().and_then(|a| string_value(*a, source));

            if function.kind() == "import"
                || (function.kind() == "identifier" && text(function, source) == "require")
            {
                if let Some(module) = first_string {
                    add_import(imports, module, None);
                }
            } else if let Some(name) = first_string {
                match classify_test_call(function, source) {
                    Some(TestCall::Suite) if arguments.len() >= 2 => {
                        suites.push(TestSuite {
                            name: name.to_string(),
                            line: start_line(node),
                            cases: Vec::new(),
                        });
                        child_suite = Some(suites.len() - 1);
                    }
                    Some(TestCall::Case { allow_no_body })
                        if arguments.len() >= 2 || allow_no_body =>
                    {
                        let case = TestCase {
                            name: name.to_string(),
                            line: start_line(node),
                        };
                        let index = enclosing.unwrap_or_else(|| {
                            *root_suite.get_or_insert_with(|| {
                                suites.push(TestSuite {
                                    name: "(root)".to_string(),
                                    line: case.line,
                                    cases: Vec::new(),
                                });
                                suites.len() - 1
                            })
                        });
                        suites[index].cases.push(case);
                    }
                    _ => {}
                }
            }
        }

        let mut children = named_children(node);
        children.reverse();
        stack.extend(children.into_iter().map(|c| (c, child_suite)));
    }

    suites
}

// Whole words only: "chai" must not match "chain", nor "jest" match "jest-dom".
fn mentions(line: &str, word: &str) -> bool {
    let is_word_char = |c: char| c.is_alphanumeric() || c == '-' || c == '_';
    line.match_indices(word).any(|(start, _)| {
        let before = line[..start].chars().next_back();
        let after = line[start + word.len()..].chars().next();
        !before.is_some_and(is_word_char) && !after.is_some_and(is_word_char)
    })
}

fn detect_framework(source: &str) -> &'static str {
    for line in source.lines().take(20) {
        if mentions(line, "vitest") {
            return "vitest";
        }
        if mentions(line, "jest") {
            return "jest";
        }
        if line.contains("@playwright/test") {
            return "playwright";
        }
        if line.contains("node:test") {
            return "node-test";
        }
        if mentions(line, "mocha") || mentions(line, "chai") {
            return "mocha";
        }
    }
    UNKNOWN_FRAMEWORK
}

#[cfg(test)]
mod tests {
    use crate::extract::{Extracted, extract};
    use crate::model::{ImportNames, Symbol};

    fn ts(source: &str) -> Extracted {
        extract("ts", source).unwrap()
    }

    fn find<'a>(result: &'a Extracted, name: &str) -> &'a Symbol {
        result
            .symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("missing symbol {name}"))
    }

    #[test]
    fn declarations_and_export_state() {
        let r = ts("export function a() {}\n\
                    function b() {}\n\
                    export default class C {}\n\
                    export interface D {}\n\
                    export type E<T> = T[];\n\
                    export enum F { X }\n\
                    export const G = 1;\n\
                    let h = 2;\n");
        for (name, kind, exported) in [
            ("a", "function", true),
            ("b", "function", false),
            ("C", "class", true),
            ("D", "interface", true),
            ("E", "type", true),
            ("F", "enum", true),
            ("G", "const", true),
            ("h", "const", false),
        ] {
            let s = find(&r, name);
            assert_eq!((s.kind.as_str(), s.exported), (kind, exported), "{name}");
        }
    }

    #[test]
    fn arrow_functions_are_functions_with_end_lines() {
        let r = ts("export const run = async () => {\n  return 1;\n};\nconst x = 1;\n");
        let run = find(&r, "run");
        assert_eq!(run.kind, "function");
        assert!(run.is_async);
        assert_eq!((run.line, run.end_line), (1, Some(3)));
        assert_eq!(find(&r, "x").kind, "const");
    }

    #[test]
    fn class_members_ignore_calls_inside_bodies() {
        let r = ts("export class Svc {\n\
                      constructor() { init(); }\n\
                      public async load() {\n    doThing();\n  }\n\
                      private hidden() {}\n\
                      #secret() {}\n\
                      handler = () => {};\n\
                      static make() {}\n\
                    }\n");
        let names: Vec<_> = r.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(!names.contains(&"doThing") && !names.contains(&"init"));
        assert!(!names.contains(&"constructor"));

        let load = find(&r, "load");
        assert_eq!(load.parent.as_deref(), Some("Svc"));
        assert!(load.exported && load.is_async);
        assert!(find(&r, "handler").exported);
        assert!(find(&r, "make").exported);
        assert!(!find(&r, "hidden").exported);
        assert!(!find(&r, "#secret").exported);
    }

    #[test]
    fn export_clauses_destructuring_and_reexports() {
        let r = ts("function a() {}\n\
                    class K { m() {} }\n\
                    export { a, K as L };\n\
                    export const { p, q: renamed, r = 1 } = obj;\n\
                    export { z } from './z';\n\
                    export * from './all';\n");
        assert!(find(&r, "a").exported);
        assert!(find(&r, "K").exported);
        assert!(find(&r, "m").exported);
        for name in ["p", "renamed", "r"] {
            assert!(find(&r, name).exported, "{name}");
        }
        assert!(!r.symbols.iter().any(|s| s.name == "q" || s.name == "obj"));

        let z = r.imports.iter().find(|i| i.source == "./z").unwrap();
        assert_eq!(z.names, Some(ImportNames::List(vec!["z".to_string()])));
        let all = r.imports.iter().find(|i| i.source == "./all").unwrap();
        assert_eq!(all.names, Some(ImportNames::star()));
    }

    #[test]
    fn namespaces_and_ambient_declarations() {
        let r = ts("export namespace NS {\n  export function inner() {}\n}\n\
                    declare function ext(): void;\n\
                    export abstract class A {\n  abstract run(): void;\n}\n");
        assert_eq!(find(&r, "NS").kind, "module");
        let inner = find(&r, "inner");
        assert_eq!(inner.parent.as_deref(), Some("NS"));
        assert!(inner.exported);
        assert_eq!(find(&r, "ext").kind, "function");
        assert_eq!(find(&r, "run").parent.as_deref(), Some("A"));
    }

    #[test]
    fn imports_of_every_shape() {
        let r = ts("import D, { a, b as c } from './one';\n\
                    import * as ns from './two';\n\
                    import './three';\n\
                    import type { T } from './one';\n\
                    import eq = require('./four');\n\
                    const lazy = () => import('./five');\n\
                    const old = require('./six');\n");
        let names = |source: &str| {
            r.imports
                .iter()
                .find(|i| i.source == source)
                .unwrap_or_else(|| panic!("missing import {source}"))
                .names
                .clone()
        };
        let list = |v: &[&str]| Some(ImportNames::List(v.iter().map(|s| s.to_string()).collect()));
        assert_eq!(names("./one"), list(&["D", "a", "b", "T"]));
        assert_eq!(names("./two"), Some(ImportNames::star()));
        assert_eq!(names("./three"), None);
        assert_eq!(names("./four"), list(&["eq"]));
        assert_eq!(names("./five"), None);
        assert_eq!(names("./six"), None);
    }

    #[test]
    fn test_blocks_nest_and_avoid_lookalike_calls() {
        let r = ts("import { describe, it } from 'vitest';\n\
                    submit('not a test', cb);\n\
                    edit('nor this', cb);\n\
                    describe('outer', () => {\n\
                      it('one', () => {});\n\
                      describe.skip('inner', () => {\n\
                        it.only('two', () => {});\n\
                        it.each(rows)('three %s', () => {});\n\
                      });\n\
                      test.todo('four');\n\
                    });\n\
                    test('five', () => {});\n");
        let tests = r.tests.unwrap();
        assert_eq!(tests.framework, "vitest");
        let summary: Vec<(&str, Vec<&str>)> = tests
            .suites
            .iter()
            .map(|s| {
                (
                    s.name.as_str(),
                    s.cases.iter().map(|c| c.name.as_str()).collect(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                ("outer", vec!["one", "four"]),
                ("inner", vec!["two", "three %s"]),
                ("(root)", vec!["five"]),
            ]
        );
    }

    #[test]
    fn playwright_suites_and_steps() {
        let r = ts("import { test } from '@playwright/test';\n\
                    test.describe('flow', () => {\n\
                      test('loads', async () => {\n\
                        await test.step('click', async () => {});\n\
                      });\n\
                    });\n");
        let tests = r.tests.unwrap();
        assert_eq!(tests.framework, "playwright");
        assert_eq!(tests.suites.len(), 1);
        assert_eq!(tests.suites[0].cases.len(), 1);
    }

    #[test]
    fn commonjs_exports_in_javascript() {
        let r = extract(
            "js",
            "function a() {}\n\
             const b = 1;\n\
             module.exports = { a, b };\n\
             exports.c = function () {};\n",
        )
        .unwrap();
        assert_eq!(r.lang, "javascript");
        assert!(find(&r, "a").exported);
        assert!(find(&r, "b").exported);
        let c = find(&r, "c");
        assert!(c.exported);
        assert_eq!(c.kind, "function");
    }

    #[test]
    fn wrapper_functions_expose_their_contents() {
        for source in [
            "(function ($) {\n  var inner = 1;\n  function helper() {}\n})(jQuery);\n",
            "(function () {\n  var inner = 1;\n  function helper() {}\n}());\n",
            "!function () {\n  var inner = 1;\n  function helper() {}\n}();\n",
            "(function () {\n  var inner = 1;\n  function helper() {}\n}).call(this);\n",
            "(function (g, f) { f(g); })(this, function (w) {\n  var inner = 1;\n  function helper() {}\n});\n",
        ] {
            let r = extract("js", source).unwrap();
            assert_eq!(find(&r, "inner").kind, "const", "{source}");
            assert_eq!(find(&r, "helper").kind, "function", "{source}");
        }
    }

    #[test]
    fn ordinary_callbacks_are_not_treated_as_wrappers() {
        let r = extract("js", "app.use(function () {\n  var local = 1;\n});\n").unwrap();
        assert!(r.symbols.is_empty());
    }

    #[test]
    fn commented_out_and_string_declarations_are_ignored() {
        let r = ts("/*\nexport function dead() {}\n*/\n\
                    const doc = `\nexport const example = 1;\n`;\n");
        let names: Vec<_> = r.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["doc"]);
    }

    #[test]
    fn framework_detection_needs_whole_words() {
        let framework = |header: &str| {
            let source = format!("{header}\ntest('a', () => {{}});\n");
            ts(&source).tests.unwrap().framework
        };
        assert_eq!(framework("// the supply chain"), "unknown");
        assert_eq!(framework("import '@testing-library/jest-dom';"), "unknown");
        assert_eq!(framework("import { expect } from '@jest/globals';"), "jest");
        assert_eq!(framework("import { expect } from 'chai';"), "mocha");
    }

    #[test]
    fn node_test_runner_is_detected() {
        let r = extract(
            "js",
            "const test = require('node:test');\ntest('works', () => {});\n",
        )
        .unwrap();
        assert_eq!(r.tests.unwrap().framework, "node-test");
    }

    #[test]
    fn tsx_parses_components() {
        let r = extract(
            "tsx",
            "export function App() {\n  return <div className=\"a\">hi</div>;\n}\n",
        )
        .unwrap();
        assert_eq!(find(&r, "App").end_line, Some(3));
    }
}
