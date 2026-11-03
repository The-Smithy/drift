use tree_sitter::Node;

use super::{Extracted, add_import, header_text, line_count, named_children, parse, symbol, text};
use crate::model::{Import, Symbol, TestCase, TestFile, TestSuite};

pub fn extract(source: &str) -> Option<Extracted> {
    let tree = parse(source, tree_sitter_gdscript::LANGUAGE.into())?;
    let root = tree.root_node();
    let mut visitor = Visitor {
        source,
        symbols: Vec::new(),
    };
    visitor.visit_block(root, None);
    let mut symbols = visitor.symbols;
    symbols.sort_by_key(|s| s.line);
    let tests = build_tests(root, &symbols, source);
    Some(Extracted {
        lang: "gdscript",
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

impl Visitor<'_> {
    fn add(&mut self, node: Node, kind: &str, parent: Option<&str>) {
        let source = self.source;
        if let Some(name) = node.child_by_field_name("name").map(|n| text(n, source)) {
            let exported = !name.starts_with('_');
            self.symbols
                .push(symbol(source, name, kind, node, exported, false, parent));
        }
    }

    fn visit_block(&mut self, node: Node, parent: Option<&str>) {
        for child in named_children(node) {
            match child.kind() {
                "class_name_statement" => self.add(child, "class", parent),
                "function_definition" => self.add(child, "function", parent),
                "constructor_definition" => {
                    self.symbols.push(symbol(
                        self.source,
                        "_init",
                        "function",
                        child,
                        false,
                        false,
                        parent,
                    ));
                }
                "signal_statement" => self.add(child, "signal", parent),
                "enum_definition" => self.add(child, "enum", parent),
                "const_statement" => self.add(child, "const", parent),
                "variable_statement" => {
                    self.add(child, "const", parent);
                    // A property's setter is a body, which doesn't belong in its signature.
                    let end = child
                        .child_by_field_name("value")
                        .or_else(|| child.child_by_field_name("setget"))
                        .map(|cut| cut.start_byte());
                    if let (Some(end), Some(entry)) = (end, self.symbols.last_mut()) {
                        entry.signature = header_text(&self.source[child.start_byte()..end]);
                    }
                }
                "class_definition" => {
                    self.add(child, "class", parent);
                    let source = self.source;
                    if let (Some(name), Some(body)) = (
                        child.child_by_field_name("name").map(|n| text(n, source)),
                        child.child_by_field_name("body"),
                    ) {
                        self.visit_block(body, Some(name));
                    }
                }
                "ERROR" => self.visit_block(child, parent),
                _ => {}
            }
        }
    }
}

fn string_of<'a>(node: Node, source: &'a str) -> Option<&'a str> {
    named_children(node)
        .into_iter()
        .find(|child| child.kind() == "string")
        .map(|string| text(string, source).trim_matches(['"', '\'']))
}

// `extends "res://base.gd"`, `preload("res://x.gd")` and `load(...)` are all dependencies.
fn collect_imports(root: Node, source: &str) -> Vec<Import> {
    let mut imports = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "extends_statement" => {
                if let Some(path) = string_of(node, source) {
                    add_import(&mut imports, path, None);
                }
            }
            "call" => {
                let is_load = named_children(node)
                    .first()
                    .is_some_and(|callee| matches!(text(*callee, source), "preload" | "load"));
                if is_load
                    && let Some(path) = node
                        .child_by_field_name("arguments")
                        .and_then(|arguments| string_of(arguments, source))
                {
                    add_import(&mut imports, path, None);
                }
            }
            _ => {}
        }
        let mut children = named_children(node);
        children.reverse();
        stack.extend(children);
    }
    imports
}

fn extends_text(node: Node, source: &str) -> Option<String> {
    named_children(node)
        .into_iter()
        .find(|child| child.kind() == "extends_statement")
        .map(|statement| text(statement, source).to_string())
}

fn framework_of(extends: &str) -> Option<&'static str> {
    if extends.contains("GutTest") || extends.contains("addons/gut/test.gd") {
        Some("gut")
    } else if extends.contains("GdUnitTestSuite") {
        Some("gdunit4")
    } else {
        None
    }
}

fn build_tests(root: Node, symbols: &[Symbol], source: &str) -> Option<TestFile> {
    let mut framework = extends_text(root, source).as_deref().and_then(framework_of);
    // Each inner class of a GUT script runs as its own suite.
    let mut test_classes: Vec<String> = Vec::new();
    for class in named_children(root)
        .into_iter()
        .filter(|node| node.kind() == "class_definition")
    {
        let inner = class
            .child_by_field_name("extends")
            .map(|extends| text(extends, source).to_string())
            .or_else(|| {
                class
                    .child_by_field_name("body")
                    .and_then(|body| extends_text(body, source))
            });
        if let Some(found) = inner.as_deref().and_then(framework_of) {
            framework.get_or_insert(found);
            if let Some(name) = class.child_by_field_name("name") {
                test_classes.push(text(name, source).to_string());
            }
        }
    }
    let framework = framework?;

    let mut suites: Vec<TestSuite> = Vec::new();
    for function in symbols
        .iter()
        .filter(|s| s.kind == "function" && s.name.starts_with("test_"))
    {
        let suite_name = match function.parent.as_deref() {
            Some(parent) if test_classes.iter().any(|class| class == parent) => parent,
            Some(_) => continue,
            None => "(module)",
        };
        let case = TestCase {
            name: function.name.clone(),
            line: function.line,
        };
        match suites.iter_mut().find(|suite| suite.name == suite_name) {
            Some(suite) => suite.cases.push(case),
            None => suites.push(TestSuite {
                name: suite_name.to_string(),
                line: function.line,
                cases: vec![case],
            }),
        }
    }
    (!suites.is_empty()).then(|| TestFile {
        framework: framework.to_string(),
        suites,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HERO: &str = "class_name Hero\nextends \"res://base.gd\"\n## Fired on death.\nsignal died(cause)\nconst MAX_HP := 10\nenum State { IDLE, RUN }\n@export var speed: float = 1.0\nvar _hp = preload(\"res://hp.gd\")\n## Moves the hero.\nfunc move(delta: float) -> void:\n\tpass\nstatic func make() -> Hero:\n\treturn Hero.new()\nfunc _private():\n\tvar x = load(\"res://x.tres\")\nfunc _init():\n\tpass\nclass Inner extends Node:\n\tfunc inner_fn():\n\t\tpass\n";

    #[test]
    fn finds_all_top_level_definitions() {
        let found: Vec<(String, String, bool, Option<String>)> = extract(HERO)
            .unwrap()
            .symbols
            .into_iter()
            .map(|s| (s.name, s.kind, s.exported, s.parent))
            .collect();
        let row = |name: &str, kind: &str, exported: bool, parent: Option<&str>| {
            (
                name.to_string(),
                kind.to_string(),
                exported,
                parent.map(str::to_string),
            )
        };
        assert_eq!(
            found,
            vec![
                row("Hero", "class", true, None),
                row("died", "signal", true, None),
                row("MAX_HP", "const", true, None),
                row("State", "enum", true, None),
                row("speed", "const", true, None),
                row("_hp", "const", false, None),
                row("move", "function", true, None),
                row("make", "function", true, None),
                row("_private", "function", false, None),
                row("_init", "function", false, None),
                row("Inner", "class", true, None),
                row("inner_fn", "function", true, Some("Inner")),
            ]
        );
    }

    #[test]
    fn signature_range_and_doc() {
        let symbols = extract(HERO).unwrap().symbols;
        let of = |name: &str| symbols.iter().find(|s| s.name == name).unwrap();
        assert_eq!(
            of("move").signature.as_deref(),
            Some("func move(delta: float) -> void")
        );
        assert_eq!(of("move").doc.as_deref(), Some("Moves the hero."));
        assert_eq!((of("move").line, of("move").end_line), (10, Some(11)));
        assert_eq!(
            of("speed").signature.as_deref(),
            Some("@export var speed: float")
        );
    }

    #[test]
    fn a_setter_stays_out_of_the_signature() {
        let source = "@export var scene: PackedScene:\n\tset(value):\n\t\tscene = value\nvar x := 1:\n\tget:\n\t\treturn 1\n";
        let symbols = extract(source).unwrap().symbols;
        assert_eq!(
            symbols[0].signature.as_deref(),
            Some("@export var scene: PackedScene")
        );
        assert_eq!(symbols[0].end_line, Some(3));
        assert_eq!(symbols[1].signature.as_deref(), Some("var x"));
    }

    #[test]
    fn finds_extends_preload_and_load() {
        let imports: Vec<String> = extract(HERO)
            .unwrap()
            .imports
            .into_iter()
            .map(|i| i.source)
            .collect();
        assert_eq!(
            imports,
            vec!["res://base.gd", "res://hp.gd", "res://x.tres"]
        );
    }

    #[test]
    fn finds_gut_tests_at_the_top_and_in_inner_classes() {
        let source = "extends GutTest\nfunc test_one():\n\tassert_eq(1, 1)\nfunc helper():\n\tpass\nclass TestInner:\n\textends GutTest\n\tfunc test_two():\n\t\tpass\n";
        let tests = extract(source).unwrap().tests.unwrap();
        assert_eq!(tests.framework, "gut");
        let suites: Vec<(&str, usize)> = tests
            .suites
            .iter()
            .map(|s| (s.name.as_str(), s.cases.len()))
            .collect();
        assert_eq!(suites, vec![("(module)", 1), ("TestInner", 1)]);
    }

    #[test]
    fn finds_gdunit_tests() {
        let tests = extract("extends GdUnitTestSuite\nfunc test_a():\n\tpass\n")
            .unwrap()
            .tests
            .unwrap();
        assert_eq!(tests.framework, "gdunit4");
    }

    #[test]
    fn a_function_named_test_in_ordinary_code_is_not_a_test() {
        assert!(
            extract("extends Node\nfunc test_thing():\n\tpass\n")
                .unwrap()
                .tests
                .is_none()
        );
    }
}
