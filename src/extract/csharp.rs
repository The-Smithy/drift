use tree_sitter::Node;

use super::{
    Extracted, add_import, first_comment_line, header_text, is_comment, line_count, named_children,
    parse, symbol, text,
};
use crate::model::{Import, Symbol, TestCase, TestFile, TestSuite};

pub fn extract(source: &str) -> Option<Extracted> {
    let tree = parse(source, tree_sitter_c_sharp::LANGUAGE.into())?;
    let mut visitor = Visitor {
        source,
        symbols: Vec::new(),
        imports: Vec::new(),
        tests: Vec::new(),
    };
    visitor.visit_block(tree.root_node(), None);
    let mut symbols = visitor.symbols;
    symbols.sort_by_key(|s| s.line);
    let tests = build_tests(visitor.tests);
    Some(Extracted {
        lang: "csharp",
        lines: line_count(source),
        symbols,
        imports: visitor.imports,
        tests,
    })
}

struct FoundTest {
    framework: &'static str,
    suite: String,
    suite_line: u32,
    case: TestCase,
}

struct Visitor<'a> {
    source: &'a str,
    symbols: Vec<Symbol>,
    imports: Vec<Import>,
    tests: Vec<FoundTest>,
}

fn test_framework(attribute: &str) -> Option<&'static str> {
    let short = attribute.rsplit('.').next().unwrap_or(attribute);
    match short {
        "Fact" | "Theory" | "SkippableFact" | "SkippableTheory" => Some("xunit"),
        "Test" | "TestCase" | "TestCaseSource" | "Theory_" => Some("nunit"),
        "TestMethod" | "DataTestMethod" => Some("mstest"),
        _ => None,
    }
}

fn attributes<'a>(node: Node, source: &'a str) -> Vec<&'a str> {
    named_children(node)
        .into_iter()
        .filter(|child| child.kind() == "attribute_list")
        .flat_map(named_children)
        .filter(|child| child.kind() == "attribute")
        .filter_map(|attribute| attribute.child_by_field_name("name"))
        .map(|name| text(name, source))
        .collect()
}

fn modifiers<'a>(node: Node, source: &'a str) -> Vec<&'a str> {
    named_children(node)
        .into_iter()
        .filter(|child| child.kind() == "modifier")
        .map(|modifier| text(modifier, source))
        .collect()
}

// Where the header starts, after any `[Attribute]` lines.
fn header_start(node: Node) -> usize {
    named_children(node)
        .into_iter()
        .find(|child| child.kind() != "attribute_list" && !is_comment(*child))
        .map_or(node.start_byte(), |child| child.start_byte())
}

// C# doc comments are one node per line, and the first is usually a bare `<summary>`.
fn doc_comment(node: Node, source: &str) -> Option<String> {
    let mut lines = Vec::new();
    let mut below = node;
    let mut current = node.prev_named_sibling();
    while let Some(sibling) = current {
        if !is_comment(sibling) || sibling.end_position().row + 1 < below.start_position().row {
            break;
        }
        lines.push(text(sibling, source));
        below = sibling;
        current = sibling.prev_named_sibling();
    }
    lines.reverse();
    lines.into_iter().find_map(first_comment_line)
}

impl<'a> Visitor<'a> {
    fn visit_block(&mut self, node: Node, parent: Option<&'a str>) {
        // A file-scoped namespace applies to everything after it.
        let mut parent = parent;
        for child in named_children(node) {
            match child.kind() {
                "using_directive" => self.using(child),
                "file_scoped_namespace_declaration" => {
                    self.namespace(child, parent);
                    let source = self.source;
                    parent = child
                        .child_by_field_name("name")
                        .map(|name| text(name, source))
                        .or(parent);
                }
                "namespace_declaration" => self.namespace(child, parent),
                "class_declaration" | "record_declaration" => {
                    self.type_with_members(child, "class", parent, false)
                }
                "struct_declaration" | "record_struct_declaration" => {
                    self.type_with_members(child, "struct", parent, false)
                }
                "interface_declaration" => {
                    self.type_with_members(child, "interface", parent, false)
                }
                "enum_declaration" => self.simple(child, "enum", parent, false),
                "delegate_declaration" => self.simple(child, "type", parent, false),
                "preproc_if" | "preproc_else" | "preproc_elif" | "preproc_region" | "ERROR" => {
                    self.visit_block(child, parent)
                }
                _ => {}
            }
        }
    }

    fn using(&mut self, node: Node) {
        // The target is the last name: in `using Alias = Foo.Bar;`, that's Foo.Bar.
        let target = named_children(node).into_iter().rfind(|child| {
            matches!(
                child.kind(),
                "identifier" | "qualified_name" | "alias_qualified_name"
            )
        });
        if let Some(target) = target {
            add_import(&mut self.imports, text(target, self.source), None);
        }
    }

    fn namespace(&mut self, node: Node, parent: Option<&'a str>) {
        let source = self.source;
        let Some(name) = node.child_by_field_name("name").map(|n| text(n, source)) else {
            return;
        };
        let mut entry = symbol(source, name, "module", node, true, false, parent);
        let end = node
            .child_by_field_name("body")
            .map_or(node.end_byte(), |body| body.start_byte());
        entry.signature = header_text(&source[node.start_byte()..end]);
        self.symbols.push(entry);
        if let Some(body) = node.child_by_field_name("body") {
            self.visit_block(body, Some(name));
        }
    }

    // Types are `internal` by default, members `private`, and interface members `public`.
    fn is_exported(node: Node, source: &str, member: bool, in_interface: bool) -> bool {
        let found = modifiers(node, source);
        if found.contains(&"private") {
            return false;
        }
        in_interface
            || !member
            || found
                .iter()
                .any(|m| matches!(*m, "public" | "protected" | "internal"))
    }

    fn entry(
        &self,
        node: Node,
        name: &str,
        kind: &str,
        parent: Option<&str>,
        exported: bool,
        end: usize,
    ) -> Symbol {
        let source = self.source;
        let mut entry = symbol(source, name, kind, node, exported, false, parent);
        entry.is_async = modifiers(node, source).contains(&"async");
        entry.signature = header_text(&source[header_start(node)..end]);
        entry.doc = doc_comment(node, source);
        let found = attributes(node, source);
        if !found.is_empty() {
            entry.decorators = Some(found.into_iter().map(str::to_string).collect());
        }
        entry
    }

    fn simple(&mut self, node: Node, kind: &str, parent: Option<&str>, member: bool) {
        let source = self.source;
        let Some(name) = node.child_by_field_name("name").map(|n| text(n, source)) else {
            return;
        };
        let exported = Self::is_exported(node, source, member, false);
        let end = node
            .child_by_field_name("body")
            .map_or(node.end_byte(), |body| body.start_byte());
        let entry = self.entry(node, name, kind, parent, exported, end);
        self.symbols.push(entry);
    }

    fn type_with_members(&mut self, node: Node, kind: &str, parent: Option<&str>, member: bool) {
        let source = self.source;
        let Some(name) = node.child_by_field_name("name").map(|n| text(n, source)) else {
            return;
        };
        self.simple(node, kind, parent, member);
        let Some(body) = node.child_by_field_name("body") else {
            return;
        };
        let in_interface = kind == "interface";
        let suite_line = self.symbols.last().map_or(1, |s| s.line);
        self.members(body, name, suite_line, in_interface);
    }

    fn members(&mut self, body: Node, name: &str, suite_line: u32, in_interface: bool) {
        for child in named_children(body) {
            match child.kind() {
                "class_declaration" | "record_declaration" => {
                    self.type_with_members(child, "class", Some(name), true)
                }
                "struct_declaration" | "record_struct_declaration" => {
                    self.type_with_members(child, "struct", Some(name), true)
                }
                "interface_declaration" => {
                    self.type_with_members(child, "interface", Some(name), true)
                }
                "enum_declaration" => self.simple(child, "enum", Some(name), true),
                "delegate_declaration" => self.simple(child, "type", Some(name), true),
                "method_declaration" | "constructor_declaration" => {
                    self.method(child, name, suite_line, in_interface)
                }
                "property_declaration" => self.property(child, name, in_interface),
                "field_declaration" => self.constant(child, name),
                // Members inside `#if` and `#region` are still members.
                "preproc_if" | "preproc_else" | "preproc_elif" | "preproc_region" | "ERROR" => {
                    self.members(child, name, suite_line, in_interface);
                }
                _ => {}
            }
        }
    }

    fn method(&mut self, node: Node, parent: &str, suite_line: u32, in_interface: bool) {
        let source = self.source;
        let Some(name) = node.child_by_field_name("name").map(|n| text(n, source)) else {
            return;
        };
        let exported = Self::is_exported(node, source, true, in_interface);
        let end = node
            .child_by_field_name("body")
            .map_or(node.end_byte(), |body| body.start_byte());
        let entry = self.entry(node, name, "function", Some(parent), exported, end);
        if let Some(framework) = attributes(node, source)
            .into_iter()
            .find_map(test_framework)
        {
            self.tests.push(FoundTest {
                framework,
                suite: parent.to_string(),
                suite_line,
                case: TestCase {
                    name: name.to_string(),
                    line: entry.line,
                },
            });
        }
        self.symbols.push(entry);
    }

    fn property(&mut self, node: Node, parent: &str, in_interface: bool) {
        let source = self.source;
        let Some(name) = node.child_by_field_name("name").map(|n| text(n, source)) else {
            return;
        };
        let exported = Self::is_exported(node, source, true, in_interface);
        let end = node
            .child_by_field_name("accessors")
            .or_else(|| node.child_by_field_name("value"))
            .map_or(node.end_byte(), |body| body.start_byte());
        let entry = self.entry(node, name, "property", Some(parent), exported, end);
        self.symbols.push(entry);
    }

    // Only constants: other fields are members, which the index leaves out.
    fn constant(&mut self, node: Node, parent: &str) {
        let source = self.source;
        let found = modifiers(node, source);
        let constant =
            found.contains(&"const") || (found.contains(&"static") && found.contains(&"readonly"));
        if !constant {
            return;
        }
        let Some(declaration) = named_children(node)
            .into_iter()
            .find(|child| child.kind() == "variable_declaration")
        else {
            return;
        };
        let exported = Self::is_exported(node, source, true, false);
        for declarator in named_children(declaration)
            .into_iter()
            .filter(|child| child.kind() == "variable_declarator")
        {
            let Some(name) = declarator
                .child_by_field_name("name")
                .or_else(|| named_children(declarator).into_iter().next())
                .map(|n| text(n, source))
            else {
                continue;
            };
            let entry = self.entry(
                node,
                name,
                "const",
                Some(parent),
                exported,
                declarator.end_byte(),
            );
            self.symbols.push(entry);
        }
    }
}

fn build_tests(found: Vec<FoundTest>) -> Option<TestFile> {
    let framework = found.first()?.framework;
    let mut suites: Vec<TestSuite> = Vec::new();
    for test in found {
        match suites.iter_mut().find(|suite| suite.name == test.suite) {
            Some(suite) => suite.cases.push(test.case),
            None => suites.push(TestSuite {
                name: test.suite,
                line: test.suite_line,
                cases: vec![test.case],
            }),
        }
    }
    Some(TestFile {
        framework: framework.to_string(),
        suites,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "using System;\nusing static System.Math;\nusing Alias = Foo.Bar;\nnamespace App.Core {\n  /// <summary>\n  /// A service.\n  /// </summary>\n  public class Service<T> : Base, IFoo {\n    public const int Max = 1;\n    private int _count;\n    public string Name { get; set; }\n    public event Action Done;\n    public Service(int a) {}\n    /// <summary>Runs.</summary>\n    [Obsolete]\n    public async Task<int> RunAsync(int a) { return 1; }\n    internal void Hidden() {}\n    private void Secret() {}\n    void DefaultPrivate() {}\n    public int Expr() => 1;\n    class Nested { public void N() {} }\n  }\n  public interface IFoo { void Bar(); int Baz { get; } }\n  public struct P { public int X; }\n  public record R(int A);\n  public enum E { A }\n  public delegate void D(int a);\n  internal static class Ext { public static void X(this int a) {} }\n}\n";

    type Row = (String, String, bool, Option<String>);

    fn rows(source: &str) -> Vec<Row> {
        extract(source)
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

    #[test]
    fn finds_types_and_members() {
        let core = Some("App.Core");
        let service = Some("Service");
        assert_eq!(
            rows(SOURCE),
            vec![
                row("App.Core", "module", true, None),
                row("Service", "class", true, core),
                row("Max", "const", true, service),
                row("Name", "property", true, service),
                row("Service", "function", true, service),
                row("RunAsync", "function", true, service),
                row("Hidden", "function", true, service),
                row("Secret", "function", false, service),
                row("DefaultPrivate", "function", false, service),
                row("Expr", "function", true, service),
                row("Nested", "class", false, service),
                row("N", "function", true, Some("Nested")),
                row("IFoo", "interface", true, core),
                row("Bar", "function", true, Some("IFoo")),
                row("Baz", "property", true, Some("IFoo")),
                row("P", "struct", true, core),
                row("R", "class", true, core),
                row("E", "enum", true, core),
                row("D", "type", true, core),
                row("Ext", "class", true, core),
                row("X", "function", true, Some("Ext")),
            ]
        );
    }

    #[test]
    fn signature_doc_and_async() {
        let symbols = extract(SOURCE).unwrap().symbols;
        let of = |name: &str| symbols.iter().find(|s| s.name == name).unwrap();
        let run = of("RunAsync");
        assert_eq!(
            run.signature.as_deref(),
            Some("public async Task<int> RunAsync(int a)")
        );
        assert_eq!(run.doc.as_deref(), Some("Runs."));
        assert!(run.is_async);
        assert_eq!(of("Service").doc.as_deref(), Some("A service."));
        assert_eq!(
            of("Service").signature.as_deref(),
            Some("public class Service<T> : Base, IFoo")
        );
        assert_eq!(of("Name").signature.as_deref(), Some("public string Name"));
        assert_eq!(
            of("Max").signature.as_deref(),
            Some("public const int Max = 1")
        );
    }

    #[test]
    fn members_inside_conditional_blocks_count() {
        let source = "public class A {\n#if NET\n  public void One() {}\n#else\n  public void Two() {}\n#endif\n#region Things\n  public void Three() {}\n#endregion\n}\n";
        let names: Vec<String> = rows(source).into_iter().map(|r| r.0).collect();
        for expected in ["One", "Two", "Three"] {
            assert!(
                names.iter().any(|n| n == expected),
                "{expected} in {names:?}"
            );
        }
    }

    #[test]
    fn finds_usings() {
        let imports: Vec<String> = extract(SOURCE)
            .unwrap()
            .imports
            .into_iter()
            .map(|i| i.source)
            .collect();
        assert_eq!(imports, vec!["System", "System.Math", "Foo.Bar"]);
    }

    #[test]
    fn a_file_scoped_namespace_applies_to_what_follows() {
        let found = rows("namespace Scoped;\nclass NoMod { void Default() {} }\n");
        assert_eq!(found[0], row("Scoped", "module", true, None));
        assert_eq!(found[1], row("NoMod", "class", true, Some("Scoped")));
    }

    #[test]
    fn finds_tests_by_attribute() {
        for (attribute, framework) in [
            ("Fact", "xunit"),
            ("Theory", "xunit"),
            ("Test", "nunit"),
            ("TestMethod", "mstest"),
        ] {
            let source = format!(
                "public class MyTests {{\n  [{attribute}] public void A() {{}}\n  public void Helper() {{}}\n}}\n"
            );
            let tests = extract(&source).unwrap().tests.unwrap();
            assert_eq!(tests.framework, framework);
            assert_eq!(tests.suites[0].name, "MyTests");
            assert_eq!(tests.suites[0].cases.len(), 1);
            assert_eq!(tests.suites[0].cases[0].line, 2);
        }
    }

    #[test]
    fn ordinary_classes_have_no_tests() {
        assert!(
            extract("class A { void TestThing() {} }\n")
                .unwrap()
                .tests
                .is_none()
        );
    }
}
