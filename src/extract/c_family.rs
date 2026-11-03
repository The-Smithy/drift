//! C and C++. The two grammars share node names, so one visitor handles both.

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::Regex;
use tree_sitter::{Language, Node};

use super::{Extracted, add_import, header_text, line_count, named_children, parse, symbol, text};
use crate::model::{Import, ImportNames, Symbol, TestCase, TestFile, TestSuite};

static LOOKS_LIKE_CPP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?m)^\s*(?:class\s+\w|namespace\b|template\s*<|public:|private:|using\s+\w)|\bstd::",
    )
    .expect("valid regex")
});

// A `.h` file can be either language, and C++ syntax would break the C grammar.
pub fn language_for(extension: &str, source: &str) -> (Language, bool) {
    let cpp = match extension {
        "c" => false,
        "h" => LOOKS_LIKE_CPP.is_match(source),
        _ => true,
    };
    let language = if cpp {
        tree_sitter_cpp::LANGUAGE.into()
    } else {
        tree_sitter_c::LANGUAGE.into()
    };
    (language, cpp)
}

// `class ENGINE_API Actor` and `UPROPERTY(EditAnywhere)` aren't C++ until the preprocessor has
// run, and they derail the parse of everything after them.
static CLASS_MACRO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:class|struct)\s+([A-Z][A-Z0-9_]*)\s+[A-Za-z_]").expect("valid regex")
});
static EXPORT_MACRO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^[ \t]*((?:[A-Z][A-Z0-9]*_)*[A-Z0-9]*(?:API|EXPORT|EXPORTS|DLL|DECL|PUBLIC))\s+[A-Za-z_]")
        .expect("valid regex")
});
static BODY_MACRO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^[ \t]*((?:UFUNCTION|UPROPERTY|UCLASS|USTRUCT|UENUM|UINTERFACE|UMETA|GENERATED_BODY|GENERATED_UCLASS_BODY|GENERATED_USTRUCT_BODY|GENERATED_IINTERFACE_BODY|Q_OBJECT|Q_GADGET|Q_PROPERTY)\b)")
        .expect("valid regex")
});

// Spaces rather than removal, so every position still matches the original source.
fn mask_macros(source: &str) -> Cow<'_, str> {
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for pattern in [&*CLASS_MACRO, &*EXPORT_MACRO] {
        for found in pattern.captures_iter(source) {
            if let Some(name) = found.get(1) {
                ranges.push((name.start(), name.end()));
            }
        }
    }
    for found in BODY_MACRO.captures_iter(source) {
        if let Some(name) = found.get(1) {
            ranges.push((name.start(), end_of_arguments(source, name.end())));
        }
    }
    if ranges.is_empty() {
        return Cow::Borrowed(source);
    }
    let mut bytes = source.as_bytes().to_vec();
    for (start, end) in ranges {
        for byte in &mut bytes[start..end] {
            if !matches!(*byte, b'\n' | b'\r') {
                *byte = b' ';
            }
        }
    }
    // Only whole characters within ASCII-delimited ranges change, so this stays valid UTF-8.
    Cow::Owned(String::from_utf8_lossy(&bytes).into_owned())
}

// The end of `(...)` right after a macro's name, if it has one.
fn end_of_arguments(source: &str, after_name: usize) -> usize {
    let bytes = source.as_bytes();
    let mut at = after_name;
    while bytes.get(at).is_some_and(|b| matches!(*b, b' ' | b'\t')) {
        at += 1;
    }
    if bytes.get(at) != Some(&b'(') {
        return after_name;
    }
    let mut depth = 0;
    let mut quoted = false;
    while let Some(&byte) = bytes.get(at) {
        match byte {
            b'"' => quoted = !quoted,
            b'(' if !quoted => depth += 1,
            b')' if !quoted => {
                depth -= 1;
                if depth == 0 {
                    return at + 1;
                }
            }
            _ => {}
        }
        at += 1;
    }
    after_name
}

pub fn extract(extension: &str, source: &str) -> Option<Extracted> {
    let (language, cpp) = language_for(extension, source);
    let masked = mask_macros(source);
    let tree = parse(&masked, language)?;
    let mut visitor = Visitor {
        source,
        cpp,
        symbols: Vec::new(),
        imports: Vec::new(),
    };
    let scope = Scope {
        parent: None,
        public: true,
        in_class: false,
        internal: false,
    };
    visitor.visit_block(tree.root_node(), scope);
    let mut symbols = visitor.symbols;
    symbols.sort_by_key(|s| s.line);
    Some(Extracted {
        lang: if cpp { "cpp" } else { "c" },
        lines: line_count(source),
        symbols,
        imports: visitor.imports,
        tests: build_tests(source),
    })
}

#[derive(Clone, Copy)]
struct Scope<'a> {
    parent: Option<&'a str>,
    public: bool,
    in_class: bool,
    // Inside an anonymous namespace, where nothing is visible to other files.
    internal: bool,
}

struct Visitor<'a> {
    source: &'a str,
    cpp: bool,
    symbols: Vec<Symbol>,
    imports: Vec<Import>,
}

// The macros that test frameworks use, which parse as functions without a return type.
const TEST_MACROS: &[&str] = &[
    "TEST",
    "TEST_F",
    "TEST_P",
    "TYPED_TEST",
    "TYPED_TEST_P",
    "GTEST_TEST",
    "TEST_CASE",
    "SCENARIO",
    "BOOST_AUTO_TEST_CASE",
    "BOOST_FIXTURE_TEST_CASE",
    "START_TEST",
    "Test",
];

fn declarator_of(node: Node) -> Option<Node> {
    node.child_by_field_name("declarator").or_else(|| {
        matches!(
            node.kind(),
            "reference_declarator" | "parenthesized_declarator" | "attributed_declarator"
        )
        .then(|| {
            named_children(node)
                .into_iter()
                .find(|child| !matches!(child.kind(), "attribute_declaration" | "ms_call_modifier"))
        })
        .flatten()
    })
}

// The name a declarator introduces, under any pointers, references and parameter lists.
fn name_node(mut node: Node) -> Option<Node> {
    loop {
        match node.kind() {
            "identifier"
            | "field_identifier"
            | "type_identifier"
            | "qualified_identifier"
            | "operator_name"
            | "destructor_name"
            | "operator_cast"
            | "template_function"
            | "template_method"
            | "primitive_type" => return Some(node),
            _ => node = declarator_of(node)?,
        }
    }
}

// True for `int f(int)`, false for a pointer to a function such as `int (*f)(int)`.
fn declares_function(mut node: Node) -> bool {
    loop {
        match node.kind() {
            "function_declarator" => {
                return node
                    .child_by_field_name("declarator")
                    .is_none_or(|inner| inner.kind() != "parenthesized_declarator");
            }
            _ => match declarator_of(node) {
                Some(inner) => node = inner,
                None => return false,
            },
        }
    }
}

// `Widget<int>::Inner::run` is `run` in `Inner`.
fn split_qualified(name: &str) -> (Option<&str>, &str) {
    let mut depth = 0;
    let bytes = name.as_bytes();
    let mut last_separator = None;
    for at in 0..bytes.len() {
        match bytes[at] {
            b'<' => depth += 1,
            b'>' if depth > 0 => depth -= 1,
            b':' if depth == 0 && bytes.get(at + 1) == Some(&b':') => last_separator = Some(at),
            _ => {}
        }
    }
    let Some(at) = last_separator else {
        return (None, name);
    };
    let qualifier = &name[..at];
    let last = qualifier.rsplit("::").next().unwrap_or(qualifier);
    let parent = last.split('<').next().unwrap_or(last);
    ((!parent.is_empty()).then_some(parent), &name[at + 2..])
}

fn has_storage(node: Node, source: &str, keyword: &str) -> bool {
    named_children(node)
        .iter()
        .any(|child| child.kind() == "storage_class_specifier" && text(*child, source) == keyword)
}

fn is_include_guard(name: &str) -> bool {
    [
        "_H",
        "_H_",
        "_H__",
        "_HPP",
        "_HPP_",
        "_INCLUDED",
        "_INCLUDED_",
    ]
    .iter()
    .any(|ending| name.ends_with(ending))
}

impl<'a> Visitor<'a> {
    fn visit_block(&mut self, node: Node, scope: Scope<'a>) {
        for child in named_children(node) {
            self.visit(child, child, scope);
        }
    }

    // `outer` is the node to report: the `template <...>` around a definition, when there is one.
    fn visit(&mut self, node: Node, outer: Node, scope: Scope<'a>) {
        match node.kind() {
            "preproc_include" => self.include(node),
            "preproc_def" | "preproc_function_def" => self.define(node, scope),
            "preproc_if" | "preproc_ifdef" | "preproc_else" | "preproc_elif"
            | "preproc_elifdef" | "declaration_list" | "ERROR" => self.visit_block(node, scope),
            "linkage_specification" => {
                if let Some(body) = node.child_by_field_name("body") {
                    self.visit(body, body, scope);
                }
            }
            "namespace_definition" => self.namespace(node, scope),
            "template_declaration" => {
                for child in named_children(node) {
                    if child.kind() != "template_parameter_list" {
                        self.visit(child, outer, scope);
                    }
                }
            }
            "function_definition" => {
                let body = node.child_by_field_name("body");
                // Unbalanced braces across `#ifdef` branches can swallow the functions after one.
                if let Some(inner) = body.filter(|_| node.has_error()) {
                    self.visit_block(inner, scope);
                }
                self.function(
                    node,
                    outer,
                    node.child_by_field_name("declarator"),
                    body,
                    scope,
                );
            }
            "declaration" | "field_declaration" => self.declaration(node, outer, scope),
            "type_definition" => self.typedef(node, outer, scope),
            "alias_declaration" => {
                let source = self.source;
                if let Some(name) = node.child_by_field_name("name").map(|n| text(n, source)) {
                    let exported = scope.public && !scope.internal;
                    self.symbols.push(symbol(
                        source,
                        name,
                        "type",
                        outer,
                        exported,
                        false,
                        scope.parent,
                    ));
                }
            }
            "class_specifier" | "struct_specifier" | "union_specifier" | "enum_specifier" => {
                self.record(node, outer, scope);
            }
            _ => {}
        }
    }

    fn include(&mut self, node: Node) {
        if let Some(path) = node.child_by_field_name("path") {
            let path = text(path, self.source).trim_matches(['"', '<', '>']);
            add_import(&mut self.imports, path, None::<ImportNames>);
        }
    }

    fn define(&mut self, node: Node, scope: Scope<'a>) {
        let source = self.source;
        let Some(name) = node.child_by_field_name("name").map(|n| text(n, source)) else {
            return;
        };
        if is_include_guard(name) {
            return;
        }
        let mut entry = symbol(source, name, "macro", node, true, false, scope.parent);
        let parameters = node
            .child_by_field_name("parameters")
            .map_or("", |p| text(p, source));
        entry.signature = Some(format!("#define {name}{parameters}"));
        self.symbols.push(entry);
    }

    fn namespace(&mut self, node: Node, scope: Scope<'a>) {
        let source = self.source;
        let name = node.child_by_field_name("name").map(|n| text(n, source));
        if let Some(name) = name {
            self.symbols.push(symbol(
                source,
                name,
                "module",
                node,
                !scope.internal,
                false,
                scope.parent,
            ));
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.visit_block(
                body,
                Scope {
                    parent: name.or(scope.parent),
                    public: true,
                    in_class: false,
                    internal: scope.internal || name.is_none(),
                },
            );
        }
    }

    fn function(
        &mut self,
        node: Node,
        outer: Node,
        declarator: Option<Node>,
        body: Option<Node>,
        scope: Scope<'a>,
    ) {
        let source = self.source;
        let Some(name) = declarator.and_then(name_node).map(|n| text(n, source)) else {
            return;
        };
        let (qualifier, name) = split_qualified(name);
        if TEST_MACROS.contains(&name) {
            return;
        }
        // `static` hides a function from other files, but on a class member it means something else.
        let file_local = !scope.in_class && has_storage(node, source, "static");
        let exported = scope.public && !scope.internal && !file_local;
        let parent = qualifier.or(scope.parent);
        let mut entry = symbol(source, name, "function", outer, exported, false, parent);
        let end = body.map_or(node.end_byte(), |body| body.start_byte());
        entry.signature = header_text(&source[outer.start_byte()..end]);
        self.symbols.push(entry);
    }

    fn declaration(&mut self, node: Node, outer: Node, scope: Scope<'a>) {
        let source = self.source;
        if let Some(kind) = node.child_by_field_name("type")
            && kind.child_by_field_name("body").is_some()
        {
            self.record(kind, kind, scope);
        }
        let mut cursor = node.walk();
        let declarators: Vec<Node> = node
            .children_by_field_name("declarator", &mut cursor)
            .collect();
        for declarator in declarators {
            if declares_function(declarator) {
                self.function(node, outer, Some(declarator), None, scope);
            } else if node.kind() == "declaration" && !scope.in_class {
                let Some(name) = name_node(declarator).map(|n| text(n, source)) else {
                    continue;
                };
                let exported =
                    scope.public && !scope.internal && !has_storage(node, source, "static");
                let mut entry = symbol(source, name, "const", outer, exported, false, scope.parent);
                let end = declarator
                    .child_by_field_name("value")
                    .map_or(node.end_byte(), |value| value.start_byte());
                entry.signature = header_text(&source[outer.start_byte()..end]);
                self.symbols.push(entry);
            }
        }
    }

    fn typedef(&mut self, node: Node, outer: Node, scope: Scope<'a>) {
        let source = self.source;
        let kind_node = node.child_by_field_name("type");
        let body = kind_node.and_then(|kind| kind.child_by_field_name("body"));
        if let (Some(kind), true) = (kind_node, body.is_some()) {
            self.record(kind, kind, scope);
        }
        let mut cursor = node.walk();
        let declarators: Vec<Node> = node
            .children_by_field_name("declarator", &mut cursor)
            .collect();
        for declarator in declarators {
            let Some(name) = name_node(declarator).map(|n| text(n, source)) else {
                continue;
            };
            let named_same = kind_node
                .and_then(|kind| kind.child_by_field_name("name"))
                .is_some_and(|n| text(n, source) == name);
            if body.is_some() && named_same {
                continue;
            }
            let kind = match kind_node.filter(|_| body.is_some()).map(|kind| kind.kind()) {
                Some("enum_specifier") => "enum",
                Some(_) => "struct",
                None => "type",
            };
            let exported = scope.public && !scope.internal;
            let mut entry = symbol(source, name, kind, outer, exported, false, scope.parent);
            if let (Some(kind_node), Some(body)) = (kind_node, body) {
                let head = header_text(&source[kind_node.start_byte()..body.start_byte()])
                    .unwrap_or_default();
                entry.signature = Some(format!("typedef {head} {{…}} {name}"));
            }
            self.symbols.push(entry);
        }
    }

    // Classes, structs, unions and enums, with a body. A forward declaration defines nothing.
    fn record(&mut self, node: Node, outer: Node, scope: Scope<'a>) {
        let source = self.source;
        let Some(body) = node.child_by_field_name("body") else {
            return;
        };
        let Some(raw) = node.child_by_field_name("name").map(|n| text(n, source)) else {
            return;
        };
        let (qualifier, name) = split_qualified(raw);
        let name = name.split('<').next().unwrap_or(name);
        let kind = match node.kind() {
            "class_specifier" => "class",
            "enum_specifier" => "enum",
            _ => "struct",
        };
        let exported = scope.public && !scope.internal;
        let mut entry = symbol(
            source,
            name,
            kind,
            outer,
            exported,
            false,
            qualifier.or(scope.parent),
        );
        entry.signature = header_text(&source[outer.start_byte()..body.start_byte()]);
        self.symbols.push(entry);

        if !self.cpp || kind == "enum" {
            return;
        }
        let mut public = kind != "class";
        for member in named_children(body) {
            if member.kind() == "access_specifier" {
                public = text(member, source) != "private";
                continue;
            }
            self.visit(
                member,
                member,
                Scope {
                    parent: Some(name),
                    public,
                    in_class: true,
                    internal: scope.internal,
                },
            );
        }
    }
}

static GOOGLE_TEST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^[ \t]*(?:TEST|TEST_F|TEST_P|TYPED_TEST|TYPED_TEST_P|GTEST_TEST)\s*\(\s*(\w+)\s*,\s*(\w+)\s*\)")
        .expect("valid regex")
});
static CATCH_TEST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?m)^[ \t]*(?:TEST_CASE|SCENARIO|TEMPLATE_TEST_CASE)\s*\(\s*"([^"]*)""#)
        .expect("valid regex")
});
static BOOST_TEST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^[ \t]*BOOST_(?:AUTO|FIXTURE|DATA)_TEST_CASE\s*\(\s*(\w+)")
        .expect("valid regex")
});
static CRITERION_TEST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^[ \t]*Test\s*\(\s*(\w+)\s*,\s*(\w+)").expect("valid regex"));
static CHECK_TEST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^[ \t]*START_TEST\s*\(\s*(\w+)").expect("valid regex"));
static C_TEST_FUNCTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^[ \t]*(?:static\s+)?void\s+(test_?\w+)\s*\(").expect("valid regex")
});

fn line_at(source: &str, byte: usize) -> u32 {
    source.as_bytes()[..byte]
        .iter()
        .filter(|&&b| b == b'\n')
        .count() as u32
        + 1
}

fn grouped(source: &str, pattern: &Regex) -> Vec<TestSuite> {
    let mut suites: Vec<TestSuite> = Vec::new();
    for found in pattern.captures_iter(source) {
        let (Some(suite), Some(case)) = (found.get(1), found.get(2)) else {
            continue;
        };
        let line = line_at(source, suite.start());
        let case = TestCase {
            name: case.as_str().to_string(),
            line,
        };
        match suites.iter_mut().find(|s| s.name == suite.as_str()) {
            Some(existing) => existing.cases.push(case),
            None => suites.push(TestSuite {
                name: suite.as_str().to_string(),
                line,
                cases: vec![case],
            }),
        }
    }
    suites
}

fn flat(source: &str, pattern: &Regex) -> Vec<TestSuite> {
    let cases: Vec<TestCase> = pattern
        .captures_iter(source)
        .filter_map(|found| found.get(1))
        .map(|name| TestCase {
            name: name.as_str().to_string(),
            line: line_at(source, name.start()),
        })
        .collect();
    match cases.first().map(|case| case.line) {
        Some(line) => vec![TestSuite {
            name: "(module)".to_string(),
            line,
            cases,
        }],
        None => Vec::new(),
    }
}

fn build_tests(source: &str) -> Option<TestFile> {
    let includes = |name: &str| {
        source
            .lines()
            .filter(|line| line.trim_start().starts_with("#include"))
            .any(|line| line.contains(name))
    };
    let (framework, suites) = if includes("gtest") {
        ("googletest", grouped(source, &GOOGLE_TEST))
    } else if includes("doctest") {
        ("doctest", flat(source, &CATCH_TEST))
    } else if includes("catch") {
        ("catch2", flat(source, &CATCH_TEST))
    } else if includes("boost/test") {
        ("boost-test", flat(source, &BOOST_TEST))
    } else if includes("criterion") {
        ("criterion", grouped(source, &CRITERION_TEST))
    } else if includes("check.h") {
        ("check", flat(source, &CHECK_TEST))
    } else if includes("unity.h") {
        ("unity", flat(source, &C_TEST_FUNCTION))
    } else if includes("cmocka.h") {
        ("cmocka", flat(source, &C_TEST_FUNCTION))
    } else {
        return None;
    };
    (!suites.is_empty()).then(|| TestFile {
        framework: framework.to_string(),
        suites,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    type Row = (String, String, bool, Option<String>);

    fn rows(extension: &str, source: &str) -> Vec<Row> {
        extract(extension, source)
            .unwrap()
            .symbols
            .into_iter()
            .map(|s| (s.name, s.kind, s.exported, s.parent))
            .collect()
    }

    fn row(name: &str, kind: &str, exported: bool, parent: Option<&str>) -> Row {
        (
            name.to_string(),
            kind.to_string(),
            exported,
            parent.map(str::to_string),
        )
    }

    const C_SOURCE: &str = "#include <stdio.h>\n#include \"util/helper.h\"\n#ifndef A_H\n#define A_H\n#define MAX 10\n#define ADD(a, b) ((a) + (b))\n/** A point. */\ntypedef struct point { int x; } point_t;\ntypedef struct { int y; } anon_t;\nstruct node { int v; };\nenum color { RED };\nunion u { int a; };\ntypedef int myint;\ntypedef int (*callback)(int);\nextern int ext_var;\nstatic int counter = 0;\nint global_var = 1;\nint declared(int a);\n/* Adds. */\nstatic int add(int a, int b) { return a + b; }\nchar *name_of(struct node *n, ...) {\n  return 0;\n}\n#endif\n";

    #[test]
    fn finds_c_definitions() {
        assert_eq!(
            rows("c", C_SOURCE),
            vec![
                row("MAX", "macro", true, None),
                row("ADD", "macro", true, None),
                row("point", "struct", true, None),
                row("point_t", "struct", true, None),
                row("anon_t", "struct", true, None),
                row("node", "struct", true, None),
                row("color", "enum", true, None),
                row("u", "struct", true, None),
                row("myint", "type", true, None),
                row("callback", "type", true, None),
                row("ext_var", "const", true, None),
                row("counter", "const", false, None),
                row("global_var", "const", true, None),
                row("declared", "function", true, None),
                row("add", "function", false, None),
                row("name_of", "function", true, None),
            ]
        );
    }

    #[test]
    fn c_signature_range_doc_and_includes() {
        let extracted = extract("c", C_SOURCE).unwrap();
        let imports: Vec<&str> = extracted
            .imports
            .iter()
            .map(|i| i.source.as_str())
            .collect();
        assert_eq!(imports, vec!["stdio.h", "util/helper.h"]);
        let of = |name: &str| extracted.symbols.iter().find(|s| s.name == name).unwrap();
        assert_eq!(
            of("add").signature.as_deref(),
            Some("static int add(int a, int b)")
        );
        assert_eq!(of("add").doc.as_deref(), Some("Adds."));
        assert_eq!(of("name_of").end_line, Some(23));
        assert_eq!(of("ADD").signature.as_deref(), Some("#define ADD(a, b)"));
        assert_eq!(
            of("anon_t").signature.as_deref(),
            Some("typedef struct {…} anon_t")
        );
        assert_eq!(
            of("declared").signature.as_deref(),
            Some("int declared(int a)")
        );
        assert_eq!(
            of("global_var").signature.as_deref(),
            Some("int global_var")
        );
    }

    const CPP_SOURCE: &str = "#include <vector>\nnamespace ns { namespace inner {\n/// A widget.\ntemplate <typename T> class Widget : public Base {\npublic:\n  Widget(int a);\n  ~Widget();\n  int size() const { return 1; }\n  static T make();\nprivate:\n  int hidden() { return 0; }\n  int field_;\n};\nstruct S { void m() {} };\nenum class Color { Red };\nusing Alias = int;\ntemplate <typename T> T identity(T v) { return v; }\nint free_fn(int a) { return a; }\nint Widget<int>::outer() { return 1; }\nvoid Foo::Bar::baz() const {}\nbool operator==(const S& a, const S& b) { return true; }\n} }\nnamespace { int hidden_fn() { return 0; } }\nextern \"C\" { int c_fn(void) { return 0; } }\nclass Decl;\n";

    #[test]
    fn finds_cpp_definitions() {
        assert_eq!(
            rows("cpp", CPP_SOURCE),
            vec![
                row("ns", "module", true, None),
                row("inner", "module", true, Some("ns")),
                row("Widget", "class", true, Some("inner")),
                row("Widget", "function", true, Some("Widget")),
                row("~Widget", "function", true, Some("Widget")),
                row("size", "function", true, Some("Widget")),
                row("make", "function", true, Some("Widget")),
                row("hidden", "function", false, Some("Widget")),
                row("S", "struct", true, Some("inner")),
                row("m", "function", true, Some("S")),
                row("Color", "enum", true, Some("inner")),
                row("Alias", "type", true, Some("inner")),
                row("identity", "function", true, Some("inner")),
                row("free_fn", "function", true, Some("inner")),
                row("outer", "function", true, Some("Widget")),
                row("baz", "function", true, Some("Bar")),
                row("operator==", "function", true, Some("inner")),
                row("hidden_fn", "function", false, None),
                row("c_fn", "function", true, None),
            ]
        );
    }

    #[test]
    fn cpp_signature_keeps_the_template_and_doc() {
        let symbols = extract("cpp", CPP_SOURCE).unwrap().symbols;
        let of = |name: &str| symbols.iter().find(|s| s.name == name).unwrap();
        assert_eq!(
            of("identity").signature.as_deref(),
            Some("template <typename T> T identity(T v)")
        );
        let widget = symbols.iter().find(|s| s.kind == "class").unwrap();
        assert_eq!(widget.doc.as_deref(), Some("A widget."));
        assert_eq!(
            widget.signature.as_deref(),
            Some("template <typename T> class Widget : public Base")
        );
        assert_eq!((widget.line, widget.end_line), (4, Some(13)));
    }

    #[test]
    fn export_macros_do_not_hide_classes() {
        let source = "class LEVELDB_EXPORT DB {\n public:\n  static Status Open(int a);\n  virtual ~DB();\n  virtual Status Put(int a) = 0;\n private:\n  int hidden();\n};\nLEVELDB_EXPORT Status DestroyDB(int a);\nstruct FOO_API FStats { void Reset(); };\n";
        assert_eq!(
            rows("h", source),
            vec![
                row("DB", "class", true, None),
                row("Open", "function", true, Some("DB")),
                row("~DB", "function", true, Some("DB")),
                row("Put", "function", true, Some("DB")),
                row("hidden", "function", false, Some("DB")),
                row("DestroyDB", "function", true, None),
                row("FStats", "struct", true, None),
                row("Reset", "function", true, Some("FStats")),
            ]
        );
        let symbols = extract("h", source).unwrap().symbols;
        assert_eq!(
            symbols[0].signature.as_deref(),
            Some("class LEVELDB_EXPORT DB")
        );
    }

    #[test]
    fn unreal_macros_do_not_derail_the_parse() {
        let source = "#include \"Actor.generated.h\"\nUCLASS(Blueprintable, meta=(Tip=\"a (b)\"))\nclass ENGINE_API AHero : public AActor\n{\n\tGENERATED_BODY()\npublic:\n\tUFUNCTION(BlueprintCallable, Category=\"x\")\n\tvoid Fire(float Power);\n\tUPROPERTY(EditAnywhere)\n\tint32 Ammo;\n\tvirtual void Tick(float Delta) override;\nprivate:\n\tvoid Secret();\n};\nvoid AHero::Fire(float Power) {\n}\n";
        assert_eq!(
            rows("h", source),
            vec![
                row("AHero", "class", true, None),
                row("Fire", "function", true, Some("AHero")),
                row("Tick", "function", true, Some("AHero")),
                row("Secret", "function", false, Some("AHero")),
                row("Fire", "function", true, Some("AHero")),
            ]
        );
        let symbols = extract("h", source).unwrap().symbols;
        assert_eq!((symbols[0].line, symbols[0].end_line), (3, Some(14)));
        assert_eq!(symbols[4].line, 15);
    }

    #[test]
    fn masking_keeps_every_position() {
        let source = "UCLASS(meta=(Name=\"é\"))\nclass A_API B {};\n";
        let masked = mask_macros(source);
        assert_eq!(masked.len(), source.len());
        assert_eq!(masked.lines().count(), source.lines().count());
        assert!(!masked.contains("UCLASS") && !masked.contains("A_API"));
        assert!(masked.contains("class"));
        assert!(matches!(mask_macros("int a;\n"), Cow::Borrowed(_)));
    }

    #[test]
    fn unbalanced_ifdef_braces_do_not_hide_later_functions() {
        let source = "#ifdef A\nvoid f(int a) {\n#else\nvoid f(long a) {\n#endif\n  go();\n}\nvoid later(void) {\n}\n";
        let names: Vec<String> = rows("c", source).into_iter().map(|r| r.0).collect();
        assert!(names.iter().any(|n| n == "later"), "{names:?}");
    }

    #[test]
    fn a_header_is_parsed_by_what_it_contains() {
        assert_eq!(extract("h", "int f(void);\n").unwrap().lang, "c");
        assert_eq!(
            extract("h", "class A { public: void f(); };\n")
                .unwrap()
                .lang,
            "cpp"
        );
        assert_eq!(
            extract("h", "namespace a { void f(); }\n").unwrap().lang,
            "cpp"
        );
    }

    #[test]
    fn a_c_variable_named_class_stays_c() {
        let found = rows("h", "int class_count;\nint new(int template);\n");
        assert_eq!(found[1], row("new", "function", true, None));
    }

    #[test]
    fn finds_test_frameworks() {
        let cases = [
            (
                "#include <gtest/gtest.h>\nTEST(Suite, One) { EXPECT_EQ(1, 1); }\nTEST_F(Fix, Two) {}\n// TEST(Off, Three) {}\n",
                "googletest",
                2,
                "One",
            ),
            (
                "#include <catch2/catch_test_macros.hpp>\nTEST_CASE(\"adds\", \"[math]\") { SECTION(\"x\") {} }\n",
                "catch2",
                1,
                "adds",
            ),
            (
                "#include \"doctest.h\"\nTEST_CASE(\"works\") {}\n",
                "doctest",
                1,
                "works",
            ),
            (
                "#include <boost/test/unit_test.hpp>\nBOOST_AUTO_TEST_CASE(adds) {}\n",
                "boost-test",
                1,
                "adds",
            ),
            (
                "#include \"unity.h\"\nvoid test_adds(void) {}\nvoid helper(void) {}\n",
                "unity",
                1,
                "test_adds",
            ),
        ];
        for (source, framework, suites, first) in cases {
            let tests = extract("cpp", source).unwrap().tests.unwrap();
            assert_eq!(tests.framework, framework);
            assert_eq!(tests.suites.len(), suites);
            assert_eq!(tests.suites[0].cases[0].name, first);
        }
    }

    #[test]
    fn test_macros_are_not_functions_and_plain_code_has_no_tests() {
        let extracted = extract(
            "cpp",
            "#include <gtest/gtest.h>\nTEST(Suite, One) {}\nint real() { return 1; }\n",
        )
        .unwrap();
        let names: Vec<&str> = extracted.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["real"]);
        assert!(
            extract("c", "void test_adds(void) {}\n")
                .unwrap()
                .tests
                .is_none()
        );
    }

    #[test]
    fn test_case_lines_are_right() {
        let tests = extract("cpp", "#include <gtest/gtest.h>\n\nTEST(Suite, One) {}\n")
            .unwrap()
            .tests
            .unwrap();
        assert_eq!(tests.suites[0].cases[0].line, 3);
    }
}
