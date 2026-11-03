use std::path::Path;

use anyhow::Result;

use crate::index::read_index;
use crate::model::{Import, ImportNames, Index, Symbol, TestSuite};

const SOURCE_EXTENSIONS: &[&str] = &[
    ".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs", ".py", ".rs",
];

pub fn query(query: &str, index_path: &Path) -> Result<()> {
    let index = read_index(index_path)?;

    // Reads `files`, not `symbol_index`: compact indexes drop private symbols from the latter.
    let mut matches = find_symbols(&index, |name| name == query);
    if matches.is_empty() {
        let lowered = query.to_lowercase();
        matches = find_symbols(&index, |name| name.to_lowercase().contains(&lowered));
    }
    if matches.is_empty() {
        no_matches(&format!("No matches for '{query}'"));
    }

    for (file, symbol) in matches {
        println!(
            "  {:12} {}  {}:{}",
            symbol.kind,
            symbol.name,
            file,
            location_details(symbol)
        );
    }
    Ok(())
}

pub fn outline(file: &str, index_path: &Path) -> Result<()> {
    let index = read_index(index_path)?;
    let candidates = resolve_file(&index, file);

    let path = match candidates.as_slice() {
        [] => no_matches(&format!("No indexed file matches '{file}'")),
        [path] => *path,
        several => {
            println!("Several files match '{file}':");
            for path in several {
                println!("  {path}");
            }
            std::process::exit(1);
        }
    };

    let entry = &index.files[path];
    println!("{path}  {}, {} lines", entry.lang, entry.lines);
    for symbol in &entry.symbols {
        println!(
            "  {:12} {}  {}",
            symbol.kind,
            symbol.name,
            location_details(symbol)
        );
    }
    if !entry.imports.is_empty() {
        println!("Imports:");
        for import in &entry.imports {
            println!("  {}", describe_import(import));
        }
    }
    Ok(())
}

pub fn tests(pattern: Option<&str>, index_path: &Path) -> Result<()> {
    let index = read_index(index_path)?;

    let Some(pattern) = pattern else {
        for (path, file) in &index.tests {
            let count: usize = file.suites.iter().map(|s| s.cases.len()).sum();
            println!("  {path}  {}  {count} tests", file.framework);
        }
        println!(
            "{} tests across {} files",
            index.test_count(),
            index.tests.len()
        );
        return Ok(());
    };

    let lowered = pattern.to_lowercase();
    let contains = |text: &str| text.to_lowercase().contains(&lowered);
    let mut found = false;

    for (path, file) in &index.tests {
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

        found = true;
        println!("{path}  {}", file.framework);
        for (suite, cases) in suites {
            println!("  {}  (line {})", suite.name, suite.line);
            for case in cases {
                println!("    {}  {}", case.line, case.name);
            }
        }
    }
    if !found {
        no_matches(&format!("No tests match '{pattern}'"));
    }
    Ok(())
}

pub fn importers(module: &str, index_path: &Path) -> Result<()> {
    let index = read_index(index_path)?;
    let target = module_key(&normalize_path(module, &index.root));
    let lowered = module.to_lowercase();
    let mut found = false;

    for (path, file) in &index.files {
        for import in &file.imports {
            if import.source.to_lowercase().contains(&lowered)
                || points_at(path, &file.lang, &import.source, &target)
            {
                found = true;
                println!("  {path}  {}", describe_import(import));
            }
        }
    }
    if !found {
        no_matches(&format!("No files import '{module}'"));
    }
    Ok(())
}

fn no_matches(message: &str) -> ! {
    println!("{message}");
    std::process::exit(1);
}

fn find_symbols(index: &Index, matches: impl Fn(&str) -> bool) -> Vec<(&str, &Symbol)> {
    index
        .files
        .iter()
        .flat_map(|(path, file)| file.symbols.iter().map(move |s| (path.as_str(), s)))
        .filter(|(_, symbol)| matches(&symbol.name))
        .collect()
}

fn location_details(symbol: &Symbol) -> String {
    let range = match symbol.end_line {
        Some(end) if end != symbol.line => format!("{}-{}", symbol.line, end),
        _ => symbol.line.to_string(),
    };
    let exported = if symbol.exported { " (exported)" } else { "" };
    let parent = symbol
        .parent
        .as_ref()
        .map(|p| format!(" in {p}"))
        .unwrap_or_default();
    format!("{range}{exported}{parent}")
}

fn describe_import(import: &Import) -> String {
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

fn resolve_file<'a>(index: &'a Index, query: &str) -> Vec<&'a str> {
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

fn ends_with_path(path: &str, suffix: &str) -> bool {
    !suffix.is_empty()
        && path
            .strip_suffix(suffix)
            .is_some_and(|rest| rest.is_empty() || rest.ends_with('/'))
}

// `src/lib/auth.ts` and `src/lib/auth/index.ts` are both imported as `src/lib/auth`.
fn module_key(path: &str) -> String {
    let lowered = path.to_lowercase();
    let without_extension = SOURCE_EXTENSIONS
        .iter()
        .find_map(|extension| lowered.strip_suffix(extension))
        .unwrap_or(&lowered);
    let key = without_extension
        .strip_suffix("/index")
        .or_else(|| without_extension.strip_suffix("/__init__"))
        .unwrap_or(without_extension);
    key.trim_end_matches('/').to_string()
}

fn points_at(importer: &str, lang: &str, source: &str, target: &str) -> bool {
    let Some(resolved) = resolve_source(importer, lang, source) else {
        return false;
    };
    let resolved = module_key(&resolved);
    resolved == target || ends_with_path(target, &resolved) || ends_with_path(&resolved, target)
}

fn resolve_source(importer: &str, lang: &str, source: &str) -> Option<String> {
    if lang == "python" {
        let dots = source.chars().take_while(|c| *c == '.').count();
        let module = source[dots..].replace('.', "/");
        if dots == 0 {
            return Some(module);
        }
        let climb = "../".repeat(dots - 1);
        return join_relative(importer, &format!("./{climb}{module}"));
    }
    if lang == "rust" {
        return None;
    }
    if source.starts_with('.') {
        return join_relative(importer, source);
    }
    source
        .strip_prefix("@/")
        .or_else(|| source.strip_prefix("~/"))
        .map(str::to_string)
}

fn join_relative(importer: &str, relative: &str) -> Option<String> {
    let mut parts: Vec<&str> = importer.split('/').collect();
    parts.pop();
    for segment in relative.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::{ends_with_path, module_key, points_at};

    fn imports(importer: &str, lang: &str, source: &str, target_path: &str) -> bool {
        points_at(importer, lang, source, &module_key(target_path))
    }

    #[test]
    fn module_keys_ignore_extension_and_index_files() {
        assert_eq!(module_key("src/lib/auth.ts"), "src/lib/auth");
        assert_eq!(module_key("src/lib/auth/index.tsx"), "src/lib/auth");
        assert_eq!(module_key("pkg/__init__.py"), "pkg");
        assert_eq!(module_key("src/lib/auth"), "src/lib/auth");
    }

    #[test]
    fn suffix_matching_respects_path_boundaries() {
        assert!(ends_with_path("src/lib/auth", "lib/auth"));
        assert!(ends_with_path("src/lib/auth", "src/lib/auth"));
        assert!(!ends_with_path("src/lib/oauth", "auth"));
        assert!(!ends_with_path("src/lib/auth", ""));
    }

    #[test]
    fn relative_imports_resolve_against_the_importer() {
        let target = "src/lib/auth.ts";
        assert!(imports(
            "src/app/page.ts",
            "typescript",
            "../lib/auth",
            target
        ));
        assert!(imports("src/lib/x.ts", "typescript", "./auth", target));
        assert!(imports("src/lib/x.ts", "typescript", "./auth.js", target));
        assert!(!imports("src/app/page.ts", "typescript", "./auth", target));
        assert!(!imports("a.ts", "typescript", "../../auth", target));
    }

    #[test]
    fn index_files_and_aliases_resolve() {
        assert!(imports(
            "src/a.ts",
            "typescript",
            "./lib",
            "src/lib/index.ts"
        ));
        assert!(imports(
            "src/a.ts",
            "typescript",
            "@/lib/auth",
            "src/lib/auth.ts"
        ));
        assert!(imports(
            "src/a.ts",
            "typescript",
            "~/lib/auth",
            "src/lib/auth.ts"
        ));
        assert!(!imports(
            "src/a.ts",
            "typescript",
            "next-auth",
            "src/lib/auth.ts"
        ));
    }

    #[test]
    fn python_modules_resolve() {
        let target = "src/pkg/mod.py";
        assert!(imports("src/app.py", "python", "pkg.mod", target));
        assert!(imports("src/pkg/other.py", "python", ".mod", target));
        assert!(imports("src/pkg/sub/deep.py", "python", "..mod", target));
        assert!(!imports("src/app.py", "python", "pkg.other", target));
    }

    #[test]
    fn rust_paths_are_left_to_substring_matching() {
        assert!(!imports("src/a.rs", "rust", "crate::model", "src/model.rs"));
    }
}
