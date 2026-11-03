//! Keys in JSON and YAML, so an agent can find its way around a large config file.

use tree_sitter::Node;

use super::{
    Extracted, MAX_SIGNATURE, line_count, named_children, parse, start_line, text, truncate,
};
use crate::model::Symbol;

// Deeper keys rarely help, and data files can hold thousands of them.
const MAX_DEPTH: usize = 2;
const MAX_KEYS: usize = 300;

struct Collector<'a> {
    source: &'a str,
    symbols: Vec<Symbol>,
}

impl Collector<'_> {
    fn is_full(&self) -> bool {
        self.symbols.len() >= MAX_KEYS
    }

    fn push(&mut self, pair: Node, key: &str, value: Option<Node>, parent: Option<&str>) {
        let shown = match value {
            Some(value) if is_container(value) => "…",
            Some(value) => text(value, self.source),
            None => "",
        };
        let signature = format!(
            "{key}: {}",
            shown.split_whitespace().collect::<Vec<_>>().join(" ")
        );
        self.symbols.push(Symbol {
            name: key.to_string(),
            kind: "key".to_string(),
            line: start_line(pair),
            end_line: Some(pair.end_position().row as u32 + 1),
            exported: false,
            is_async: false,
            parent: parent.map(str::to_string),
            decorators: None,
            signature: Some(truncate(signature.trim_end(), MAX_SIGNATURE.min(100))),
            doc: None,
        });
    }

    fn json_object(&mut self, object: Node, parent: Option<&str>, depth: usize) {
        for pair in named_children(object) {
            if self.is_full() {
                return;
            }
            if pair.kind() != "pair" {
                continue;
            }
            let Some(key) = pair.child_by_field_name("key") else {
                continue;
            };
            let name = unquote(text(key, self.source));
            let value = pair.child_by_field_name("value");
            self.push(pair, name, value, parent);
            if depth < MAX_DEPTH
                && let Some(value) = value.filter(|value| value.kind() == "object")
            {
                self.json_object(value, Some(name), depth + 1);
            }
        }
    }

    fn yaml_mapping(&mut self, mapping: Node, parent: Option<&str>, depth: usize) {
        for pair in named_children(mapping) {
            if self.is_full() {
                return;
            }
            if !matches!(pair.kind(), "block_mapping_pair" | "flow_pair") {
                continue;
            }
            let Some(key) = pair.child_by_field_name("key") else {
                continue;
            };
            let name = unquote(text(key, self.source));
            let value = pair.child_by_field_name("value");
            self.push(pair, name, value, parent);
            if depth < MAX_DEPTH
                && let Some(inner) = value.and_then(yaml_mapping_of)
            {
                self.yaml_mapping(inner, Some(name), depth + 1);
            }
        }
    }
}

fn is_container(value: Node) -> bool {
    matches!(
        value.kind(),
        "object" | "array" | "block_node" | "block_mapping" | "block_sequence"
    ) || yaml_mapping_of(value).is_some()
        || named_children(value)
            .iter()
            .any(|child| child.kind() == "flow_sequence")
}

fn yaml_mapping_of(node: Node) -> Option<Node> {
    if matches!(node.kind(), "block_mapping" | "flow_mapping") {
        return Some(node);
    }
    if !matches!(node.kind(), "block_node" | "flow_node") {
        return None;
    }
    named_children(node)
        .into_iter()
        .find(|child| matches!(child.kind(), "block_mapping" | "flow_mapping"))
}

fn unquote(key: &str) -> &str {
    key.trim().trim_matches(['"', '\''])
}

pub fn extract_json(source: &str) -> Option<Extracted> {
    let tree = parse(source, tree_sitter_json::LANGUAGE.into())?;
    let mut collector = Collector {
        source,
        symbols: Vec::new(),
    };
    for child in named_children(tree.root_node()) {
        if child.kind() == "object" {
            collector.json_object(child, None, 1);
        }
    }
    Some(Extracted {
        lang: "json",
        lines: line_count(source),
        symbols: collector.symbols,
        imports: Vec::new(),
        tests: None,
    })
}

pub fn extract_yaml(source: &str) -> Option<Extracted> {
    let tree = parse(source, tree_sitter_yaml::LANGUAGE.into())?;
    let mut collector = Collector {
        source,
        symbols: Vec::new(),
    };
    for document in named_children(tree.root_node()) {
        for node in named_children(document) {
            if let Some(mapping) = yaml_mapping_of(node) {
                collector.yaml_mapping(mapping, None, 1);
            }
        }
    }
    Some(Extracted {
        lang: "yaml",
        lines: line_count(source),
        symbols: collector.symbols,
        imports: Vec::new(),
        tests: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(extracted: Extracted) -> Vec<(String, u32, u32, Option<String>)> {
        extracted
            .symbols
            .into_iter()
            .map(|s| (s.name, s.line, s.end_line.unwrap(), s.parent))
            .collect()
    }

    #[test]
    fn json_lists_keys_to_two_levels_with_ranges() {
        let source = "{\n  // a comment\n  \"name\": \"x\",\n  \"scripts\": {\n    \"build\": \"tsc\",\n    \"deep\": { \"hidden\": 1 }\n  },\n  \"list\": [ { \"no\": 1 } ]\n}\n";
        assert_eq!(
            keys(extract_json(source).unwrap()),
            vec![
                ("name".to_string(), 3, 3, None),
                ("scripts".to_string(), 4, 7, None),
                ("build".to_string(), 5, 5, Some("scripts".to_string())),
                ("deep".to_string(), 6, 6, Some("scripts".to_string())),
                ("list".to_string(), 8, 8, None),
            ]
        );
    }

    #[test]
    fn json_signature_shows_scalars_but_not_containers() {
        let symbols = extract_json("{\"a\": \"b\", \"c\": {\"d\": 1}}")
            .unwrap()
            .symbols;
        assert_eq!(symbols[0].signature.as_deref(), Some("a: \"b\""));
        assert_eq!(symbols[1].signature.as_deref(), Some("c: …"));
    }

    #[test]
    fn a_json_array_or_broken_file_is_not_a_failure() {
        assert!(extract_json("[1, 2]").unwrap().symbols.is_empty());
        assert!(extract_json("").unwrap().symbols.is_empty());
        assert!(extract_json("{\"a\": ").is_some());
    }

    #[test]
    fn yaml_handles_nesting_flow_quotes_and_documents() {
        let source = "# comment\nversion: 2\nservices:\n  web:\n    image: nginx\n  db: {image: pg}\n\"quoted key\": 1\nlist:\n  - name: a\n    run: |\n      key: not a key\n---\nsecond: doc\n";
        let found = keys(extract_yaml(source).unwrap());
        let names: Vec<(&str, Option<&str>)> = found
            .iter()
            .map(|(name, _, _, parent)| (name.as_str(), parent.as_deref()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("version", None),
                ("services", None),
                ("web", Some("services")),
                ("db", Some("services")),
                ("quoted key", None),
                ("list", None),
                ("second", None),
            ]
        );
        assert_eq!(found[1].1, 3);
        assert_eq!(found[1].2, 6);
    }

    #[test]
    fn stops_at_the_key_limit() {
        let body: Vec<String> = (0..MAX_KEYS + 50).map(|n| format!("\"k{n}\": 1")).collect();
        let source = format!("{{{}}}", body.join(","));
        assert_eq!(extract_json(&source).unwrap().symbols.len(), MAX_KEYS);
    }
}
