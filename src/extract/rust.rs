use std::sync::LazyLock;

use regex::Regex;
use tree_sitter::Node;

use super::{Extracted, add_import, line_count, named_children, parse, start_line, symbol, text};
use crate::model::{Import, ImportNames, Symbol, TestCase, TestFile, TestSuite};

const MODULE_SUITE: &str = "(module)";

// Macro bodies such as proptest! { ... } are opaque token trees to the parser.
static MACRO_TEST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"#\[test\]\s*(?:#\[[^\]]*\]\s*)*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)")
        .expect("valid macro test pattern")
});

pub fn extract(source: &str) -> Option<Extracted> {
    let tree = parse(source, tree_sitter_rust::LANGUAGE.into())?;
    let mut visitor = Visitor {
        source,
        symbols: Vec::new(),
        imports: Vec::new(),
        suites: Vec::new(),
    };
    let scope = Scope {
        parent: None,
        module: None,
        module_line: None,
        in_test_module: false,
        inherited_export: false,
    };
    visitor.visit_items(tree.root_node(), scope);

    let mut symbols = visitor.symbols;
    symbols.sort_by_key(|s| s.line);
    let tests = (!visitor.suites.is_empty()).then(|| TestFile {
        framework: "cargo-test".to_string(),
        suites: visitor.suites,
    });

    Some(Extracted {
        lang: "rust",
        lines: line_count(source),
        symbols,
        imports: visitor.imports,
        tests,
    })
}

#[derive(Clone, Copy)]
struct Scope<'a> {
    parent: Option<&'a str>,
    module: Option<&'a str>,
    module_line: Option<u32>,
    in_test_module: bool,
    // Trait items have no `pub` of their own; they are as visible as the trait.
    inherited_export: bool,
}

struct Visitor<'a> {
    source: &'a str,
    symbols: Vec<Symbol>,
    imports: Vec<Import>,
    suites: Vec<TestSuite>,
}

impl<'a> Visitor<'a> {
    fn visit_items(&mut self, node: Node, scope: Scope) {
        let mut attributes: Vec<&'a str> = Vec::new();
        for child in named_children(node) {
            match child.kind() {
                "attribute_item" => attributes.push(text(child, self.source)),
                "line_comment" | "block_comment" => {}
                _ => {
                    self.visit_item(child, scope, &attributes);
                    attributes.clear();
                }
            }
        }
    }

    fn visit_item(&mut self, node: Node, scope: Scope, attributes: &[&str]) {
        let exported = scope.inherited_export || has_visibility(node);
        match node.kind() {
            "function_item" | "function_signature_item" => {
                let Some(name) = self.name_of(node) else {
                    return;
                };
                if attributes.iter().any(|a| is_test_attribute(a)) {
                    self.add_test_case(name, start_line(node), scope);
                }
                if !scope.in_test_module {
                    let is_async = named_children(node).iter().any(|c| {
                        c.kind() == "function_modifiers" && text(*c, self.source).contains("async")
                    });
                    self.symbols.push(symbol(
                        name,
                        "function",
                        node,
                        exported,
                        is_async,
                        scope.parent,
                    ));
                }
            }
            "struct_item" | "union_item" => self.push_named(node, "struct", exported, scope),
            "enum_item" => self.push_named(node, "enum", exported, scope),
            "type_item" => self.push_named(node, "type", exported, scope),
            "const_item" | "static_item" => self.push_named(node, "const", exported, scope),
            "macro_definition" => {
                let exported = attributes.iter().any(|a| a.contains("macro_export"));
                self.push_named(node, "macro", exported, scope);
            }
            "trait_item" => {
                let Some(name) = self.name_of(node) else {
                    return;
                };
                self.push_named(node, "trait", exported, scope);
                if let Some(body) = node.child_by_field_name("body") {
                    self.visit_items(
                        body,
                        Scope {
                            parent: Some(name),
                            inherited_export: exported,
                            ..scope
                        },
                    );
                }
            }
            "impl_item" => self.visit_impl(node, scope),
            "foreign_mod_item" => {
                if let Some(body) = node.child_by_field_name("body") {
                    self.visit_items(body, scope);
                }
            }
            "mod_item" => self.visit_mod(node, scope, attributes, exported),
            "macro_invocation" => self.scan_macro_tests(node, scope),
            "expression_statement" | "ERROR" => self.visit_items(node, scope),
            "use_declaration" => {
                if let Some(argument) = node.child_by_field_name("argument") {
                    let (path, names) = parse_use(text(argument, self.source));
                    // In a test module `super` is just this file, so it is not a dependency.
                    let refers_to_this_file =
                        scope.in_test_module && (path == "super" || path.starts_with("super::"));
                    if !refers_to_this_file {
                        add_import(&mut self.imports, &path, names);
                    }
                }
            }
            _ => {}
        }
    }

    fn name_of(&self, node: Node) -> Option<&'a str> {
        node.child_by_field_name("name")
            .map(|n| text(n, self.source))
    }

    fn push_named(&mut self, node: Node, kind: &str, exported: bool, scope: Scope) {
        if scope.in_test_module {
            return;
        }
        if let Some(name) = self.name_of(node) {
            self.symbols
                .push(symbol(name, kind, node, exported, false, scope.parent));
        }
    }

    fn visit_impl(&mut self, node: Node, scope: Scope) {
        let Some(type_node) = node.child_by_field_name("type") else {
            return;
        };
        let type_name = base_type_name(type_node, self.source);
        let label = match node.child_by_field_name("trait") {
            Some(trait_node) => {
                format!(
                    "{} for {}",
                    base_type_name(trait_node, self.source),
                    type_name
                )
            }
            None => type_name.to_string(),
        };
        if !scope.in_test_module {
            self.symbols
                .push(symbol(&label, "impl", node, false, false, None));
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.visit_items(
                body,
                Scope {
                    parent: Some(type_name),
                    inherited_export: false,
                    ..scope
                },
            );
        }
    }

    fn visit_mod(&mut self, node: Node, scope: Scope, attributes: &[&str], exported: bool) {
        let Some(name) = self.name_of(node) else {
            return;
        };
        let is_test_module = attributes.iter().any(|a| is_cfg_test(a));
        if !scope.in_test_module && !is_test_module {
            self.symbols
                .push(symbol(name, "module", node, exported, false, scope.parent));
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.visit_items(
                body,
                Scope {
                    parent: Some(name),
                    module: Some(name),
                    module_line: Some(start_line(node)),
                    in_test_module: scope.in_test_module || is_test_module,
                    inherited_export: false,
                },
            );
        }
    }

    fn scan_macro_tests(&mut self, node: Node, scope: Scope) {
        let body = text(node, self.source);
        for captures in MACRO_TEST.captures_iter(body) {
            let name = captures.get(1).expect("pattern has one group");
            let offset = body[..name.start()].bytes().filter(|&b| b == b'\n').count() as u32;
            self.add_test_case(name.as_str(), start_line(node) + offset, scope);
        }
    }

    fn add_test_case(&mut self, name: &str, line: u32, scope: Scope) {
        let suite_name = scope.module.unwrap_or(MODULE_SUITE);
        let case = TestCase {
            name: name.to_string(),
            line,
        };
        match self.suites.iter_mut().find(|s| s.name == suite_name) {
            Some(suite) => suite.cases.push(case),
            None => self.suites.push(TestSuite {
                name: suite_name.to_string(),
                line: scope.module_line.unwrap_or(case.line),
                cases: vec![case],
            }),
        }
    }
}

fn has_visibility(node: Node) -> bool {
    named_children(node)
        .iter()
        .any(|c| c.kind() == "visibility_modifier")
}

fn base_type_name<'a>(node: Node, source: &'a str) -> &'a str {
    let mut current = node;
    while matches!(current.kind(), "generic_type" | "reference_type")
        && let Some(inner) = current.child_by_field_name("type")
    {
        current = inner;
    }
    text(current, source)
}

fn is_test_attribute(attribute: &str) -> bool {
    let inner = attribute
        .trim_start_matches("#[")
        .trim_end_matches(']')
        .trim();
    let path = inner.split('(').next().unwrap_or(inner).trim();
    let last = path.rsplit("::").next().unwrap_or(path);
    matches!(last, "test" | "rstest" | "test_case")
}

fn is_cfg_test(attribute: &str) -> bool {
    let compact: String = attribute.split_whitespace().collect();
    compact.starts_with("#[cfg(") && compact.contains("test") && !compact.contains("not(test")
}

fn strip_alias(item: &str) -> &str {
    item.split(" as ").next().unwrap_or(item).trim()
}

fn parse_use(raw: &str) -> (String, Option<ImportNames>) {
    let normalized = raw.split_whitespace().collect::<Vec<_>>().join(" ");

    if let Some(open) = normalized.find('{') {
        let base = normalized[..open].trim_end_matches("::").trim().to_string();
        let inner = normalized[open + 1..]
            .trim_end()
            .strip_suffix('}')
            .unwrap_or(&normalized[open + 1..]);
        return (base, Some(ImportNames::List(split_top_level(inner))));
    }

    match normalized.rsplit_once("::") {
        Some((path, "*")) => (path.to_string(), Some(ImportNames::star())),
        Some((path, item)) => (
            path.to_string(),
            Some(ImportNames::List(vec![strip_alias(item).to_string()])),
        ),
        None => (strip_alias(&normalized).to_string(), None),
    }
}

pub fn split_top_level(list: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    for (index, c) in list.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                items.push(&list[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    items.push(&list[start..]);
    items
        .into_iter()
        .map(strip_alias)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::extract::{Extracted, extract};
    use crate::model::{ImportNames, Symbol};

    fn rs(source: &str) -> Extracted {
        extract("rs", source).unwrap()
    }

    fn find<'a>(result: &'a Extracted, name: &str) -> &'a Symbol {
        result
            .symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("missing symbol {name}"))
    }

    #[test]
    fn top_level_items() {
        let r = rs("pub fn a() {}\n\
                    pub(crate) async unsafe fn b() {}\n\
                    fn c() {}\n\
                    pub struct S;\n\
                    enum E { X }\n\
                    pub trait T {}\n\
                    pub type Alias = u32;\n\
                    pub const LIMIT: u32 = 1;\n\
                    static GLOBAL: u32 = 2;\n\
                    #[macro_export]\nmacro_rules! exported { () => {} }\n\
                    macro_rules! local { () => {} }\n\
                    pub mod external;\n");
        assert!(find(&r, "a").exported);
        let b = find(&r, "b");
        assert!(b.exported && b.is_async);
        assert!(!find(&r, "c").exported);
        for (name, kind) in [
            ("S", "struct"),
            ("E", "enum"),
            ("T", "trait"),
            ("Alias", "type"),
            ("LIMIT", "const"),
            ("GLOBAL", "const"),
            ("exported", "macro"),
            ("local", "macro"),
            ("external", "module"),
        ] {
            assert_eq!(find(&r, name).kind, kind, "{name}");
        }
        assert!(find(&r, "exported").exported);
        assert!(!find(&r, "local").exported);
    }

    #[test]
    fn impl_labels_and_method_visibility() {
        let r = rs(
            "impl<T> Foo<T> {\n    pub fn new() -> Self { todo!() }\n    fn hidden(&self) {}\n}\n\
             impl fmt::Display for Foo<u8> {\n    fn fmt(&self) {}\n}\n",
        );
        assert_eq!(find(&r, "Foo").kind, "impl");
        assert_eq!(find(&r, "fmt::Display for Foo").kind, "impl");

        let new = find(&r, "new");
        assert_eq!(new.parent.as_deref(), Some("Foo"));
        assert!(new.exported);
        assert_eq!(new.line, 2);
        assert!(!find(&r, "hidden").exported);
        assert!(!find(&r, "fmt").exported);
    }

    #[test]
    fn trait_items_inherit_visibility() {
        let r = rs(
            "pub trait Run {\n    fn go(&self);\n    fn done(&self) {}\n}\n\
                    trait Private {\n    fn nope(&self);\n}\n",
        );
        let go = find(&r, "go");
        assert_eq!(go.parent.as_deref(), Some("Run"));
        assert!(go.exported);
        assert!(find(&r, "done").exported);
        assert!(!find(&r, "nope").exported);
    }

    #[test]
    fn inline_modules_nest() {
        let r = rs("pub mod outer {\n    pub fn inner() {}\n}\n");
        assert_eq!(find(&r, "outer").kind, "module");
        assert_eq!(find(&r, "inner").parent.as_deref(), Some("outer"));
    }

    #[test]
    fn use_declarations() {
        let r = rs("use std::fmt;\n\
                    use std::io::{self, Read as R, Write};\n\
                    pub use crate::a::{b::{c, d}, e};\n\
                    use glob::*;\n\
                    use anyhow;\n\
                    use std::fmt::Debug;\n");
        let names = |source: &str| {
            r.imports
                .iter()
                .find(|i| i.source == source)
                .unwrap_or_else(|| panic!("missing import {source}"))
                .names
                .clone()
        };
        let list = |v: &[&str]| Some(ImportNames::List(v.iter().map(|s| s.to_string()).collect()));
        assert_eq!(names("std"), list(&["fmt"]));
        assert_eq!(names("std::io"), list(&["self", "Read", "Write"]));
        assert_eq!(names("crate::a"), list(&["b::{c, d}", "e"]));
        assert_eq!(names("glob"), Some(ImportNames::star()));
        assert_eq!(names("anyhow"), None);
        assert_eq!(names("std::fmt"), list(&["Debug"]));
    }

    #[test]
    fn test_modules_record_real_dependencies_only() {
        let source = [
            "#[cfg(test)]",
            "mod tests {",
            "    use super::*;",
            "    use super::helper::Thing;",
            "    use crate::fixtures::Sample;",
            "}",
            "",
        ]
        .join("\n");
        let r = rs(&source);
        let sources: Vec<_> = r.imports.iter().map(|i| i.source.as_str()).collect();
        assert_eq!(sources, vec!["crate::fixtures"]);
    }

    #[test]
    fn tests_in_cfg_test_modules_stay_out_of_symbols() {
        let source = [
            "pub fn real() {}",
            "#[cfg(test)]",
            "mod tests {",
            "    use super::*;",
            "    fn helper() {}",
            "    #[test]",
            "    fn plain() {}",
            "    #[tokio::test(flavor = \"multi_thread\")]",
            "    async fn asynchronous() {}",
            "}",
            "",
        ]
        .join("\n");
        let r = rs(&source);
        let names: Vec<_> = r.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["real"]);
        assert!(r.imports.is_empty());

        let tests = r.tests.unwrap();
        assert_eq!(tests.framework, "cargo-test");
        assert_eq!(tests.suites.len(), 1);
        let suite = &tests.suites[0];
        assert_eq!((suite.name.as_str(), suite.line), ("tests", 3));
        let cases: Vec<_> = suite
            .cases
            .iter()
            .map(|c| (c.name.as_str(), c.line))
            .collect();
        assert_eq!(cases, vec![("plain", 7), ("asynchronous", 9)]);
    }

    #[test]
    fn tests_inside_macro_bodies() {
        let source = [
            "proptest! {",
            "    #[test]",
            "    fn holds(x in 0..5) {",
            "        prop_assert!(x < 5);",
            "    }",
            "    #[test]",
            "    #[ignore]",
            "    fn slow(y in 0..5) {}",
            "}",
            "vec![1];",
            "",
        ]
        .join("\n");
        let tests = rs(&source).tests.unwrap();
        let cases: Vec<_> = tests.suites[0]
            .cases
            .iter()
            .map(|c| (c.name.as_str(), c.line))
            .collect();
        assert_eq!(cases, vec![("holds", 3), ("slow", 8)]);
    }

    #[test]
    fn integration_style_tests_at_top_level() {
        let r = rs("#[test]\n// note\nfn works() {}\nfn not_a_test() {}\n");
        let tests = r.tests.as_ref().unwrap();
        assert_eq!(tests.suites[0].name, "(module)");
        assert_eq!(tests.suites[0].cases.len(), 1);
        find(&r, "works");
        find(&r, "not_a_test");
    }
}
