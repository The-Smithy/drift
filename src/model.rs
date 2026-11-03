use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: &str = "1.4";

fn default_true() -> bool {
    true
}

fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Symbol {
    pub name: String,
    pub kind: String,
    pub line: u32,
    #[serde(default)]
    pub end_line: Option<u32>,
    #[serde(default)]
    pub exported: bool,
    #[serde(default, rename = "async")]
    pub is_async: bool,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decorators: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum ImportNames {
    List(Vec<String>),
    // Always the literal "*".
    Star(String),
}

impl ImportNames {
    pub fn star() -> Self {
        ImportNames::Star("*".to_string())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Import {
    pub source: String,
    #[serde(default)]
    pub names: Option<ImportNames>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TestCase {
    pub name: String,
    pub line: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TestSuite {
    pub name: String,
    pub line: u32,
    pub cases: Vec<TestCase>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TestFile {
    pub framework: String,
    pub suites: Vec<TestSuite>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FileEntry {
    pub lang: String,
    pub lines: u32,
    pub symbols: Vec<Symbol>,
    #[serde(default)]
    pub imports: Vec<Import>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SymbolLocation {
    pub file: String,
    pub line: u32,
    #[serde(default)]
    pub end_line: Option<u32>,
    pub kind: String,
    // Compact indexes only list exported symbols, so they omit this field.
    #[serde(default = "default_true")]
    pub exported: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Index {
    pub version: String,
    pub repo: String,
    pub root: String,
    pub generated_at: String,
    pub languages: Vec<String>,
    // Repeated when a lookup refreshes the index, so a refresh matches the original run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore_dirs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore_files: Vec<String>,
    pub files: BTreeMap<String, FileEntry>,
    pub tests: BTreeMap<String, TestFile>,
    pub symbol_index: BTreeMap<String, Vec<SymbolLocation>>,
}

impl Index {
    pub fn symbol_count(&self) -> usize {
        self.files.values().map(|f| f.symbols.len()).sum()
    }

    pub fn test_count(&self) -> usize {
        self.tests
            .values()
            .flat_map(|t| &t.suites)
            .map(|s| s.cases.len())
            .sum()
    }

    pub fn to_compact(&self) -> CompactIndex<'_> {
        CompactIndex {
            version: &self.version,
            repo: &self.repo,
            root: &self.root,
            generated_at: &self.generated_at,
            languages: &self.languages,
            ignore_dirs: &self.ignore_dirs,
            ignore_files: &self.ignore_files,
            files: self
                .files
                .iter()
                .map(|(path, file)| (path.as_str(), CompactFile::from(file)))
                .collect(),
            tests: &self.tests,
            symbol_index: self
                .symbol_index
                .iter()
                .filter_map(|(name, locations)| {
                    let exported: Vec<_> = locations
                        .iter()
                        .filter(|l| l.exported)
                        .map(|l| CompactLocation {
                            file: &l.file,
                            line: l.line,
                            end_line: l.end_line,
                            kind: &l.kind,
                        })
                        .collect();
                    (!exported.is_empty()).then_some((name.as_str(), exported))
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
pub struct CompactIndex<'a> {
    version: &'a str,
    repo: &'a str,
    root: &'a str,
    generated_at: &'a str,
    languages: &'a [String],
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    ignore_dirs: &'a [String],
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    ignore_files: &'a [String],
    files: BTreeMap<&'a str, CompactFile<'a>>,
    tests: &'a BTreeMap<String, TestFile>,
    symbol_index: BTreeMap<&'a str, Vec<CompactLocation<'a>>>,
}

#[derive(Serialize)]
struct CompactFile<'a> {
    lang: &'a str,
    lines: u32,
    symbols: Vec<CompactSymbol<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    imports: Vec<CompactImport<'a>>,
}

impl<'a> From<&'a FileEntry> for CompactFile<'a> {
    fn from(file: &'a FileEntry) -> Self {
        CompactFile {
            lang: &file.lang,
            lines: file.lines,
            symbols: file
                .symbols
                .iter()
                .map(|s| CompactSymbol {
                    name: &s.name,
                    kind: &s.kind,
                    line: s.line,
                    end_line: s.end_line,
                    exported: s.exported,
                    is_async: s.is_async,
                    parent: s.parent.as_deref(),
                    decorators: s.decorators.as_deref().filter(|d| !d.is_empty()),
                    signature: s.signature.as_deref(),
                    doc: s.doc.as_deref(),
                })
                .collect(),
            imports: file
                .imports
                .iter()
                .map(|i| CompactImport {
                    source: &i.source,
                    names: i.names.as_ref(),
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct CompactSymbol<'a> {
    name: &'a str,
    kind: &'a str,
    line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_line: Option<u32>,
    #[serde(skip_serializing_if = "is_false")]
    exported: bool,
    #[serde(rename = "async", skip_serializing_if = "is_false")]
    is_async: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    decorators: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    signature: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    doc: Option<&'a str>,
}

#[derive(Serialize)]
struct CompactImport<'a> {
    source: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    names: Option<&'a ImportNames>,
}

#[derive(Serialize)]
struct CompactLocation<'a> {
    file: &'a str,
    line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_line: Option<u32>,
    kind: &'a str,
}
