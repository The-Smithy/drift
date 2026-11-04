use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::path::Path;

use rayon::prelude::*;

use crate::extract;
use crate::imports::{
    RustLocation, declares_module, ends_with_path, module_key, points_at, rust_path_reaches,
    use_paths,
};
use crate::index::read_source;
use crate::model::{Import, ImportNames, Index, Symbol, TestFile, TestSuite};

// Default output caps. Reading a definition is what `show` is for, so it gets more room.
pub const LOOKUP_CAP: usize = 4 * 1024;
pub const SOURCE_CAP: usize = 12 * 1024;
pub const BUNDLE_CAP: usize = 6 * 1024;

const MAX_LINE_TEXT: usize = 100;
const CONTEXT_ITEMS: usize = 8;

#[derive(Default, Clone)]
pub struct View {
    pub limit: Option<usize>,
    pub all: bool,
    pub within: Option<String>,
}

impl View {
    pub fn includes(&self, path: &str) -> bool {
        let Some(within) = &self.within else {
            return true;
        };
        let within = within.replace('\\', "/");
        let within = within.trim_start_matches("./").trim_end_matches('/');
        within.is_empty()
            || path == within
            || path
                .strip_prefix(within)
                .is_some_and(|rest| rest.starts_with('/'))
    }
}

pub struct Answer {
    pub lines: Vec<String>,
    pub found: bool,
}

impl Answer {
    fn found(lines: Vec<String>) -> Self {
        Answer { lines, found: true }
    }

    fn missing(message: String) -> Self {
        Answer {
            lines: vec![message],
            found: false,
        }
    }
}

pub fn counted(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

pub fn fit(lines: &[String], view: &View, cap: usize) -> String {
    let mut out = String::new();
    let mut shown = 0;
    for line in lines {
        let over_limit = !view.all && view.limit.is_some_and(|limit| shown >= limit);
        let over_cap =
            !view.all && view.limit.is_none() && shown > 0 && out.len() + line.len() + 1 > cap;
        if over_limit || over_cap {
            break;
        }
        out.push_str(line);
        out.push('\n');
        shown += 1;
    }
    if shown < lines.len() {
        out.push_str(&format!(
            "... {} more {}. Narrow with --in <path> or --limit <n>, or use --all.\n",
            lines.len() - shown,
            if lines.len() - shown == 1 {
                "line"
            } else {
                "lines"
            }
        ));
    }
    out
}

pub fn deliver(answer: Answer, view: &View, cap: usize) {
    print!("{}", fit(&answer.lines, view, cap));
    if !answer.found {
        std::process::exit(1);
    }
}

// Source first, then tests, then code an agent rarely means: fixtures, generated files, migrations.
pub fn path_tier(path: &str) -> u8 {
    let lowered = path.to_lowercase();
    let mut segments = lowered.split('/');
    let file = segments.next_back().unwrap_or("");
    let directories: Vec<&str> = segments.collect();

    const LOW: &[&str] = &[
        "golden",
        "addons",
        "fixtures",
        "fixture",
        "__fixtures__",
        "__snapshots__",
        "generated",
        "third_party",
        "migrations",
        "template",
        "templates",
        "examples",
        "example",
        "public",
    ];
    if directories.iter().any(|d| LOW.contains(d))
        || file.ends_with(".d.ts")
        || file.ends_with(".designer.cs")
        || file.ends_with(".g.cs")
        || crate::extract::is_data_file(file)
        || file.contains(".gen.")
        || file.contains(".generated.")
    {
        return 2;
    }
    const TESTS: &[&str] = &["tests", "test", "__tests__", "spec", "e2e"];
    let stem = file.split('.').next().unwrap_or("");
    if directories.iter().any(|d| TESTS.contains(d))
        || file.contains(".test.")
        || file.contains(".spec.")
        || stem.starts_with("test_")
        || stem.ends_with("_test")
    {
        return 1;
    }
    0
}

fn is_migration(path: &str) -> bool {
    path.split('/').any(|segment| segment == "migrations")
}

fn compare_paths(a: &str, b: &str) -> Ordering {
    // The newest migration is the definition that's live now.
    if is_migration(a) && is_migration(b) {
        b.cmp(a)
    } else {
        a.cmp(b)
    }
}

type Located<'a> = (&'a str, &'a Symbol);

fn rank(matches: &mut [Located]) {
    matches.sort_by(|(a_path, a), (b_path, b)| {
        (path_tier(a_path), !a.exported)
            .cmp(&(path_tier(b_path), !b.exported))
            .then_with(|| compare_paths(a_path, b_path))
            .then_with(|| a.line.cmp(&b.line))
    });
}

fn symbols_where<'a>(
    index: &'a Index,
    view: &View,
    accept: impl Fn(&str, &Symbol) -> bool,
) -> Vec<Located<'a>> {
    let mut matches: Vec<Located> = index
        .files
        .iter()
        .filter(|(path, _)| view.includes(path))
        .flat_map(|(path, file)| file.symbols.iter().map(move |s| (path.as_str(), s)))
        .filter(|(path, symbol)| accept(path, symbol))
        .collect();
    rank(&mut matches);
    matches
}

// `name`, `Parent.name`, `Parent::name`, `path:name` or `path:line`.
fn exact_matches<'a>(index: &'a Index, selector: &str, view: &View) -> Vec<Located<'a>> {
    let spelled_with_dots = selector.contains("::").then(|| selector.replace("::", "."));
    let selector = spelled_with_dots.as_deref().unwrap_or(selector);
    let (file, name) = match selector.rsplit_once(':') {
        Some((file, name)) if !file.is_empty() && !name.is_empty() && !file.ends_with(':') => {
            (Some(file), name)
        }
        _ => (None, selector),
    };
    let wanted_file = file.map(|file| normalize_path(file, &index.root));
    // Overloads share a name, so a line number is the only way to pick one.
    if let (Some(wanted), Ok(line)) = (&wanted_file, name.parse::<u32>()) {
        return symbols_where(index, view, |path, symbol| {
            symbol.line == line && (path == wanted || ends_with_path(path, wanted))
        });
    }
    let (parent, name) = match name.rsplit_once('.') {
        Some((parent, name)) if !name.is_empty() => (Some(parent), name),
        _ => (None, name),
    };
    symbols_where(index, view, |path, symbol| {
        symbol.name == name
            && symbol.kind != "section"
            && parent.is_none_or(|parent| symbol.parent.as_deref() == Some(parent))
            && wanted_file
                .as_ref()
                .is_none_or(|wanted| path == wanted || ends_with_path(path, wanted))
    })
}

fn range(symbol: &Symbol) -> String {
    match symbol.end_line {
        Some(end) if end != symbol.line => format!("{}-{}", symbol.line, end),
        _ => symbol.line.to_string(),
    }
}

fn details(symbol: &Symbol) -> String {
    let exported = if symbol.exported { " (exported)" } else { "" };
    let parent = symbol
        .parent
        .as_ref()
        .map(|p| format!(" in {p}"))
        .unwrap_or_default();
    format!("{exported}{parent}")
}

fn symbol_line(path: &str, symbol: &Symbol) -> String {
    format!(
        "{} {}  {}:{}{}",
        symbol.kind,
        symbol.name,
        path,
        range(symbol),
        details(symbol)
    )
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let current = row[j + 1];
            row[j + 1] = (previous + usize::from(ca != *cb))
                .min(row[j] + 1)
                .min(current + 1);
            previous = current;
        }
    }
    row[b.len()]
}

fn suggestions(index: &Index, name: &str) -> Vec<String> {
    let lowered = name.to_lowercase();
    let allowed = (lowered.chars().count() / 3).max(2);
    let names: BTreeSet<&str> = index
        .files
        .values()
        .flat_map(|file| &file.symbols)
        .filter(|symbol| symbol.kind != "section")
        .map(|symbol| symbol.name.as_str())
        .collect();
    let mut close: Vec<(usize, &str)> = names
        .into_iter()
        .filter_map(|candidate| {
            let lower = candidate.to_lowercase();
            // Length can only add to the distance, so this skips most names cheaply.
            if lower.chars().count().abs_diff(lowered.chars().count()) > allowed {
                return None;
            }
            let distance = edit_distance(&lowered, &lower);
            (distance <= allowed).then_some((distance, candidate))
        })
        .collect();
    close.sort();
    close
        .into_iter()
        .take(3)
        .map(|(_, name)| name.to_string())
        .collect()
}

fn miss_message(index: &Index, name: &str) -> String {
    let close = suggestions(index, name);
    if close.is_empty() {
        format!("No matches for '{name}'")
    } else {
        format!(
            "No matches for '{name}'. Did you mean: {}?",
            close.join(", ")
        )
    }
}

pub fn query(index: &Index, names: &[String], view: &View) -> Answer {
    let grouped = names.len() > 1;
    let mut lines = Vec::new();
    let mut found = false;

    for name in names {
        let mut matches = exact_matches(index, name, view);
        if matches.is_empty() {
            let lowered = name.to_lowercase();
            matches = symbols_where(index, view, |_, symbol| {
                symbol.kind != "section" && symbol.name.to_lowercase().contains(&lowered)
            });
        }
        if grouped {
            lines.push(format!("{name}:"));
        }
        let indent = if grouped { "  " } else { "" };
        if matches.is_empty() {
            lines.push(format!("{indent}{}", miss_message(index, name)));
            continue;
        }
        found = true;
        lines.extend(
            matches
                .iter()
                .map(|(path, symbol)| format!("{indent}{}", symbol_line(path, symbol))),
        );
    }
    Answer { lines, found }
}

fn single_file<'a>(index: &'a Index, file: &str) -> Result<&'a str, Answer> {
    let candidates = resolve_file(index, file);
    match candidates.as_slice() {
        [] => Err(Answer::missing(format!("No indexed file matches '{file}'"))),
        [path] => Ok(path),
        several => {
            let mut lines = vec![format!("Several files match '{file}':")];
            lines.extend(several.iter().map(|path| format!("  {path}")));
            Err(Answer {
                lines,
                found: false,
            })
        }
    }
}

pub fn outline(index: &Index, file: &str, signatures: bool) -> Answer {
    let path = match single_file(index, file) {
        Ok(path) => path,
        Err(answer) => return answer,
    };
    let entry = &index.files[path];
    let mut lines = vec![format!("{path}  {}, {} lines", entry.lang, entry.lines)];
    for symbol in &entry.symbols {
        let shown = signatures
            .then_some(symbol.signature.as_deref())
            .flatten()
            .map_or_else(
                || format!("{} {}", symbol.kind, symbol.name),
                str::to_string,
            );
        lines.push(format!("  {shown}  {}{}", range(symbol), details(symbol)));
    }
    if !entry.imports.is_empty() {
        lines.push("Imports:".to_string());
        lines.extend(
            entry
                .imports
                .iter()
                .map(|import| format!("  {}", describe_import(import))),
        );
    }
    Answer::found(lines)
}

fn test_count(file: &TestFile) -> usize {
    file.suites.iter().map(|suite| suite.cases.len()).sum()
}

fn test_summary(path: &str, file: &TestFile) -> String {
    format!(
        "{path}  {}  {}",
        file.framework,
        counted(test_count(file), "test")
    )
}

pub fn tests(index: &Index, pattern: Option<&str>, view: &View) -> Answer {
    let Some(pattern) = pattern else {
        let in_scope: Vec<_> = index
            .tests
            .iter()
            .filter(|(path, _)| view.includes(path))
            .collect();
        let total: usize = in_scope.iter().map(|(_, file)| test_count(file)).sum();
        let mut lines: Vec<String> = in_scope
            .iter()
            .map(|(path, file)| test_summary(path, file))
            .collect();
        // A total printed last would be lost when the list is cut, so it goes first.
        lines.insert(
            0,
            format!(
                "{} across {}",
                counted(total, "test"),
                counted(in_scope.len(), "file")
            ),
        );
        return Answer::found(lines);
    };

    let lowered = pattern.to_lowercase();
    let contains = |text: &str| text.to_lowercase().contains(&lowered);
    let mut lines = Vec::new();

    for (path, file) in index.tests.iter().filter(|(path, _)| view.includes(path)) {
        let path_matches = contains(path);
        let suites: Vec<(&TestSuite, Vec<_>)> = file
            .suites
            .iter()
            .filter_map(|suite| {
                let whole_suite = path_matches || contains(&suite.name);
                let cases: Vec<_> = suite
                    .cases
                    .iter()
                    .filter(|case| whole_suite || contains(&case.name))
                    .collect();
                (whole_suite || !cases.is_empty()).then_some((suite, cases))
            })
            .collect();
        if suites.is_empty() {
            continue;
        }
        lines.push(format!("{path}  {}", file.framework));
        for (suite, cases) in suites {
            lines.push(format!("  {}  (line {})", suite.name, suite.line));
            lines.extend(
                cases
                    .into_iter()
                    .map(|case| format!("    {}  {}", case.line, case.name)),
            );
        }
    }
    if lines.is_empty() {
        return Answer::missing(format!("No tests match '{pattern}'"));
    }
    Answer::found(lines)
}

fn base_name(path: &str) -> &str {
    let file = path.rsplit('/').next().unwrap_or(path);
    let stem = file.split('.').next().unwrap_or(file);
    let stem = stem.strip_prefix("test_").unwrap_or(stem);
    ["_test", "_tests", "_spec", "Tests", "Test", "Spec"]
        .iter()
        .find_map(|suffix| stem.strip_suffix(suffix))
        .filter(|rest| !rest.is_empty())
        .unwrap_or(stem)
}

// Test files for a source file: its own inline tests, ones that import it, and ones named after it.
pub fn tests_of<'a>(index: &'a Index, path: &str) -> Vec<(&'a str, &'a TestFile)> {
    let importing: BTreeSet<String> = importers_of(index, path)
        .into_iter()
        .map(|importer| importer.path)
        .collect();
    let base = base_name(path);
    let generic = matches!(base, "index" | "mod" | "lib" | "main" | "__init__");
    index
        .tests
        .iter()
        .filter(|(test_path, _)| {
            test_path.as_str() == path
                || importing.contains(test_path.as_str())
                || (!generic && base_name(test_path) == base)
        })
        .map(|(test_path, file)| (test_path.as_str(), file))
        .collect()
}

pub fn tests_for(index: &Index, file: &str) -> Answer {
    let path = match single_file(index, file) {
        Ok(path) => path,
        Err(answer) => return answer,
    };
    let found = tests_of(index, path);
    if found.is_empty() {
        return Answer::missing(format!(
            "No tests import {path}, are named after it, or sit inside it"
        ));
    }
    Answer::found(
        found
            .into_iter()
            .map(|(test_path, test_file)| test_summary(test_path, test_file))
            .collect(),
    )
}

pub struct Importer {
    pub path: String,
    pub detail: String,
}

pub fn importers_of(index: &Index, module: &str) -> Vec<Importer> {
    let target_path = match resolve_file(index, module).as_slice() {
        [only] => only.to_string(),
        _ => normalize_path(module, &index.root),
    };
    let target = module_key(&target_path);
    let rust_target = target_path
        .ends_with(".rs")
        .then(|| RustLocation::of(&target_path, &index.repo));
    let lowered = module.to_lowercase();
    let mut found = Vec::new();

    for (path, file) in &index.files {
        let rust_context = rust_target
            .as_ref()
            .filter(|_| file.lang == "rust" && *path != target_path)
            .map(|target| (RustLocation::of(path, &index.repo), target));

        if let Some((from, target)) = &rust_context {
            for symbol in &file.symbols {
                if declares_module(symbol, from, target) {
                    found.push(Importer {
                        path: path.clone(),
                        detail: format!("mod {}", symbol.name),
                    });
                }
            }
        }
        for import in &file.imports {
            let resolves = rust_context.as_ref().is_some_and(|(from, target)| {
                use_paths(import)
                    .iter()
                    .any(|use_path| rust_path_reaches(use_path, from, target))
            });
            if resolves
                || import.source.to_lowercase().contains(&lowered)
                || points_at(path, &file.lang, import, &target)
            {
                found.push(Importer {
                    path: path.clone(),
                    detail: describe_import(import),
                });
            }
        }
    }
    found.sort_by(|a, b| {
        path_tier(&a.path)
            .cmp(&path_tier(&b.path))
            .then_with(|| a.path.cmp(&b.path))
    });
    found
}

pub fn importers(index: &Index, module: &str, view: &View) -> Answer {
    let lines: Vec<String> = importers_of(index, module)
        .into_iter()
        .filter(|importer| view.includes(&importer.path))
        .map(|importer| format!("{}  {}", importer.path, importer.detail))
        .collect();
    if lines.is_empty() {
        return Answer::missing(format!("No files import '{module}'"));
    }
    Answer::found(lines)
}

// `path:start-end`, the form `drift docs` points at.
fn show_lines(index: &Index, selector: &str) -> Option<Answer> {
    let (file, lines) = selector.rsplit_once(':')?;
    let (start, end) = lines.split_once('-')?;
    let start: usize = start.parse().ok().filter(|start| *start > 0)?;
    let end: usize = end.parse().ok().filter(|end| *end >= start)?;
    let path = match single_file(index, file) {
        Ok(path) => path,
        Err(answer) => return Some(answer),
    };
    let Some(source) = read_source(Path::new(&index.root), path) else {
        return Some(Answer::missing(format!("Can't read {path}")));
    };
    let wanted: Vec<String> = source
        .lines()
        .skip(start - 1)
        .take(end - start + 1)
        .map(str::to_string)
        .collect();
    if wanted.is_empty() {
        return Some(Answer::missing(format!("{path} has no line {start}")));
    }
    let mut output = vec![format!("{path}:{start}-{}", start + wanted.len() - 1)];
    output.extend(wanted);
    Some(Answer::found(output))
}

pub fn show(index: &Index, selector: &str, view: &View) -> Answer {
    let mut matches = exact_matches(index, selector, view);
    // A definition is what's wanted, not its one-line declaration in a header.
    matches.sort_by_key(|(_, symbol)| symbol.end_line.unwrap_or(symbol.line) == symbol.line);
    let Some(((path, symbol), others)) = matches.split_first() else {
        if let Some(answer) = show_lines(index, selector) {
            return answer;
        }
        let name = selector.rsplit([':', '.']).next().unwrap_or(selector);
        return Answer::missing(miss_message(index, name));
    };
    let Some(source) = read_source(Path::new(&index.root), path) else {
        return Answer::missing(format!("Can't read {path}"));
    };
    let start = symbol.line as usize - 1;
    let end = symbol.end_line.unwrap_or(symbol.line) as usize;

    let mut lines = vec![symbol_line(path, symbol)];
    lines.extend(
        source
            .lines()
            .skip(start)
            .take(end.saturating_sub(start))
            .map(str::to_string),
    );
    if !others.is_empty() {
        let also: Vec<String> = others
            .iter()
            .take(3)
            .map(|(path, symbol)| format!("{path}:{}", range(symbol)))
            .collect();
        let more = others.len().saturating_sub(also.len());
        let suffix = if more > 0 {
            format!(" and {more} more")
        } else {
            String::new()
        };
        lines.push(format!(
            "Also defined at {}{suffix}. Pick one with path:line.",
            also.join(", ")
        ));
    }
    Answer::found(lines)
}

fn signature_lines(path: &str, symbol: &Symbol) -> Vec<String> {
    let mut lines = vec![format!("{path}:{}{}", range(symbol), details(symbol))];
    match &symbol.signature {
        Some(signature) => lines.push(format!("  {signature}")),
        None => lines.push(format!("  {} {}", symbol.kind, symbol.name)),
    }
    if let Some(doc) = &symbol.doc {
        lines.push(format!("  {doc}"));
    }
    lines
}

pub fn sig(index: &Index, names: &[String], view: &View) -> Answer {
    let mut lines = Vec::new();
    let mut found = false;
    for name in names {
        let matches = exact_matches(index, name, view);
        if matches.is_empty() {
            lines.push(miss_message(index, name));
            continue;
        }
        found = true;
        for (path, symbol) in matches {
            lines.extend(signature_lines(path, symbol));
        }
    }
    Answer { lines, found }
}

pub fn name_words(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut previous_lower = false;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            previous_lower = false;
            continue;
        }
        if c.is_uppercase() && previous_lower {
            words.push(std::mem::take(&mut current));
        }
        previous_lower = c.is_lowercase() || c.is_numeric();
        current.extend(c.to_lowercase());
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

pub fn find(index: &Index, query: &[String], view: &View) -> Answer {
    let wanted: Vec<String> = query.iter().flat_map(|word| name_words(word)).collect();
    if wanted.is_empty() {
        return Answer::missing("Nothing to search for".to_string());
    }

    let mut scored: Vec<(u32, &str, &Symbol)> = Vec::new();
    for (path, file) in index.files.iter().filter(|(path, _)| view.includes(path)) {
        let lowered_path = path.to_lowercase();
        for symbol in file.symbols.iter().filter(|s| s.kind != "section") {
            let words = name_words(&symbol.name);
            let doc = symbol.doc.as_deref().unwrap_or("").to_lowercase();
            let mut score = 0;
            let mut all_found = true;
            for word in &wanted {
                if words.iter().any(|candidate| candidate == word) {
                    score += 4;
                } else if words
                    .iter()
                    .any(|candidate| candidate.starts_with(word.as_str()))
                {
                    score += 3;
                } else if doc.contains(word.as_str()) || lowered_path.contains(word.as_str()) {
                    score += 1;
                } else {
                    all_found = false;
                    break;
                }
            }
            if all_found {
                scored.push((score, path, symbol));
            }
        }
    }
    if scored.is_empty() {
        return Answer::missing(format!("No matches for '{}'", query.join(" ")));
    }
    scored.sort_by(|(a_score, a_path, a), (b_score, b_path, b)| {
        b_score
            .cmp(a_score)
            .then_with(|| (path_tier(a_path), !a.exported).cmp(&(path_tier(b_path), !b.exported)))
            .then_with(|| a_path.cmp(b_path))
            .then_with(|| a.line.cmp(&b.line))
    });
    Answer::found(
        scored
            .into_iter()
            .map(|(_, path, symbol)| {
                let line = symbol_line(path, symbol);
                match &symbol.doc {
                    Some(doc) => format!("{line}  # {doc}"),
                    None => line,
                }
            })
            .collect(),
    )
}

pub struct FileReferences {
    pub path: String,
    pub hits: Vec<Reference>,
}

pub struct Reference {
    pub line: u32,
    pub enclosing: Option<String>,
    pub text: String,
}

pub fn innermost_symbol(symbols: &[Symbol], line: u32) -> Option<&Symbol> {
    symbols
        .iter()
        .filter(|symbol| symbol.line <= line && symbol.end_line.is_some_and(|end| line <= end))
        .min_by_key(|symbol| symbol.end_line.unwrap_or(symbol.line) - symbol.line)
}

pub fn references(index: &Index, name: &str, view: &View) -> Vec<FileReferences> {
    let name = name.rsplit(['.', ':']).next().unwrap_or(name);
    let root = Path::new(&index.root);
    let entries: Vec<_> = index
        .files
        .iter()
        .filter(|(path, _)| view.includes(path))
        .collect();

    let mut found: Vec<FileReferences> = entries
        .par_iter()
        .filter_map(|(path, file)| {
            let source = read_source(root, path)?;
            if !source.contains(name) {
                return None;
            }
            let extension = Path::new(path.as_str())
                .extension()?
                .to_str()?
                .to_lowercase();
            let lines: Vec<&str> = source.lines().collect();
            let hits: Vec<Reference> = extract::reference_lines(&extension, &source, name)
                .into_iter()
                .filter(|line| {
                    !file
                        .symbols
                        .iter()
                        .any(|symbol| symbol.name == name && symbol.line == *line)
                })
                .map(|line| Reference {
                    line,
                    enclosing: innermost_symbol(&file.symbols, line)
                        .map(|symbol| symbol.name.clone()),
                    text: lines
                        .get(line as usize - 1)
                        .map(|text| shorten(text.trim()))
                        .unwrap_or_default(),
                })
                .collect();
            (!hits.is_empty()).then(|| FileReferences {
                path: (*path).clone(),
                hits,
            })
        })
        .collect();
    found.sort_by(|a, b| {
        path_tier(&a.path)
            .cmp(&path_tier(&b.path))
            .then_with(|| a.path.cmp(&b.path))
    });
    found
}

fn shorten(text: &str) -> String {
    if text.chars().count() <= MAX_LINE_TEXT {
        return text.to_string();
    }
    let kept: String = text.chars().take(MAX_LINE_TEXT).collect();
    format!("{kept}...")
}

fn reference_line(hit: &Reference) -> String {
    match &hit.enclosing {
        Some(enclosing) => format!("  {} {enclosing}: {}", hit.line, hit.text),
        None => format!("  {} {}", hit.line, hit.text),
    }
}

pub fn refs(index: &Index, name: &str, view: &View) -> Answer {
    let found = references(index, name, view);
    let total: usize = found.iter().map(|file| file.hits.len()).sum();
    if total == 0 {
        let known = !exact_matches(index, name, view).is_empty();
        let message = if known {
            format!("No references to '{name}' outside its definition")
        } else {
            miss_message(index, name)
        };
        return Answer::missing(message);
    }
    let mut lines = vec![format!(
        "{} in {}",
        counted(total, "reference"),
        counted(found.len(), "file")
    )];
    for file in &found {
        lines.push(file.path.clone());
        lines.extend(file.hits.iter().map(reference_line));
    }
    Answer::found(lines)
}

fn is_file_target(index: &Index, target: &str) -> bool {
    let looks_like_path = target.contains('/') || target.contains('\\') || target.contains('.');
    let resolves = resolve_file(index, target).len() == 1;
    resolves && (looks_like_path || exact_matches(index, target, &View::default()).is_empty())
}

fn push_limited(lines: &mut Vec<String>, heading: String, items: Vec<String>, hint: &str) {
    if items.is_empty() {
        return;
    }
    lines.push(heading);
    let total = items.len();
    lines.extend(
        items
            .into_iter()
            .take(CONTEXT_ITEMS)
            .map(|item| format!("  {item}")),
    );
    if total > CONTEXT_ITEMS {
        lines.push(format!("  ... {} more: {hint}", total - CONTEXT_ITEMS));
    }
}

fn related(lines: &mut Vec<String>, index: &Index, path: &str) {
    let mut importers: Vec<String> = importers_of(index, path)
        .into_iter()
        .map(|importer| importer.path)
        .collect();
    importers.dedup();
    push_limited(
        lines,
        format!("Imported by {}:", counted(importers.len(), "file")),
        importers,
        &format!("drift importers {path}"),
    );
    let tests: Vec<String> = tests_of(index, path)
        .into_iter()
        .map(|(test_path, file)| test_summary(test_path, file))
        .collect();
    push_limited(
        lines,
        "Tests:".to_string(),
        tests,
        &format!("drift tests --for {path}"),
    );
}

pub fn context(index: &Index, target: &str, view: &View) -> Answer {
    if is_file_target(index, target) {
        let path = resolve_file(index, target)[0];
        let entry = &index.files[path];
        let mut lines = vec![format!("{path}  {}, {} lines", entry.lang, entry.lines)];
        for symbol in entry.symbols.iter().filter(|symbol| symbol.exported) {
            let shown = symbol
                .signature
                .clone()
                .unwrap_or_else(|| format!("{} {}", symbol.kind, symbol.name));
            lines.push(format!("  {shown}  {}", range(symbol)));
        }
        related(&mut lines, index, path);
        return Answer::found(lines);
    }

    let matches = exact_matches(index, target, view);
    let Some(((path, symbol), others)) = matches.split_first() else {
        return Answer::missing(miss_message(index, target));
    };
    let mut lines = signature_lines(path, symbol);
    if !others.is_empty() {
        lines.push(format!(
            "Also defined in {} more: drift query {}",
            counted(others.len(), "place"),
            symbol.name
        ));
    }
    let callers: Vec<String> = references(index, &symbol.name, view)
        .iter()
        .flat_map(|file| {
            file.hits.iter().map(|hit| match &hit.enclosing {
                Some(enclosing) => format!("{}:{} {enclosing}", file.path, hit.line),
                None => format!("{}:{}", file.path, hit.line),
            })
        })
        .collect();
    push_limited(
        &mut lines,
        format!("Used in {}:", counted(callers.len(), "place")),
        callers,
        &format!("drift refs {}", symbol.name),
    );
    related(&mut lines, index, path);
    Answer::found(lines)
}

pub fn describe_import(import: &Import) -> String {
    match &import.names {
        Some(ImportNames::List(names)) if !names.is_empty() => {
            format!("{}: {}", import.source, names.join(", "))
        }
        Some(ImportNames::Star(_)) => format!("{}: *", import.source),
        _ => import.source.clone(),
    }
}

fn normalize_path(path: &str, root: &str) -> String {
    let path = path.replace('\\', "/");
    let root = format!("{}/", root.replace('\\', "/"));
    let relative = path.strip_prefix(&root).unwrap_or(&path);
    relative.strip_prefix("./").unwrap_or(relative).to_string()
}

pub fn resolve_file<'a>(index: &'a Index, query: &str) -> Vec<&'a str> {
    let wanted = normalize_path(query, &index.root);
    if let Some((path, _)) = index.files.get_key_value(&wanted) {
        return vec![path.as_str()];
    }
    index
        .files
        .keys()
        .filter(|path| ends_with_path(path, &wanted))
        .map(String::as_str)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{View, edit_distance, fit, name_words, path_tier};

    #[test]
    fn output_stops_at_the_cap_and_says_what_was_left_out() {
        let lines: Vec<String> = (0..100).map(|i| format!("line {i}")).collect();
        let out = fit(&lines, &View::default(), 50);
        assert!(out.len() < 150, "{out}");
        assert!(out.contains("more lines"), "{out}");
    }

    #[test]
    fn a_single_long_line_is_still_shown() {
        let lines = vec!["x".repeat(500)];
        assert_eq!(fit(&lines, &View::default(), 50).lines().count(), 1);
    }

    #[test]
    fn limit_and_all_override_the_cap() {
        let lines: Vec<String> = (0..100).map(|i| format!("line {i}")).collect();
        let limited = View {
            limit: Some(2),
            ..View::default()
        };
        assert_eq!(fit(&lines, &limited, 50).lines().count(), 3);
        let everything = View {
            all: true,
            ..View::default()
        };
        assert_eq!(fit(&lines, &everything, 50).lines().count(), 100);
    }

    #[test]
    fn scope_matches_whole_path_segments() {
        let view = View {
            within: Some("src/app/".to_string()),
            ..View::default()
        };
        assert!(view.includes("src/app/page.ts"));
        assert!(!view.includes("src/application/page.ts"));
    }

    #[test]
    fn tiers_put_source_before_tests_before_noise() {
        assert_eq!(path_tier("src/lib/auth.ts"), 0);
        assert_eq!(path_tier("src/lib/auth.test.ts"), 1);
        assert_eq!(path_tier("tests/auth.rs"), 1);
        assert_eq!(path_tier("pkg/test_auth.py"), 1);
        assert_eq!(path_tier("supabase/migrations/001_init.sql"), 2);
        assert_eq!(path_tier("tests/fixtures/auth.ts"), 2);
        assert_eq!(path_tier("src/types.d.ts"), 2);
        assert_eq!(path_tier("src/latest/contest.ts"), 0);
    }

    #[test]
    fn names_split_into_words() {
        assert_eq!(name_words("enforceRateLimit"), ["enforce", "rate", "limit"]);
        assert_eq!(name_words("rate_limit_v2"), ["rate", "limit", "v2"]);
        assert_eq!(name_words("HTTPServer"), ["httpserver"]);
    }

    #[test]
    fn edit_distance_counts_changes() {
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("same", "same"), 0);
    }
}
