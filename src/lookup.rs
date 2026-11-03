use std::path::Path;

use anyhow::Result;

use crate::extract::split_top_level;
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
    let target_path = match resolve_file(&index, module).as_slice() {
        [only] => only.to_string(),
        _ => normalize_path(module, &index.root),
    };
    let target = module_key(&target_path);
    let rust_target = target_path
        .ends_with(".rs")
        .then(|| RustLocation::of(&target_path, &index.repo));
    let lowered = module.to_lowercase();
    let mut found = false;

    for (path, file) in &index.files {
        let rust_context = rust_target
            .as_ref()
            .filter(|_| file.lang == "rust" && *path != target_path)
            .map(|target| (RustLocation::of(path, &index.repo), target));

        if let Some((from, target)) = &rust_context {
            for symbol in &file.symbols {
                if declares_module(symbol, from, target) {
                    found = true;
                    println!("  {path}  mod {}", symbol.name);
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

fn points_at(importer: &str, lang: &str, import: &Import, target: &str) -> bool {
    let Some(resolved) = resolve_source(importer, lang, &import.source) else {
        return false;
    };
    let matches = |candidate: &str| {
        let key = module_key(candidate);
        key == target || ends_with_path(target, &key) || ends_with_path(&key, target)
    };
    if matches(&resolved) {
        return true;
    }
    // `from . import sibling` names the module in the imported names, not the source.
    match &import.names {
        Some(ImportNames::List(names)) if lang == "python" => names
            .iter()
            .any(|name| matches(format!("{resolved}/{name}").trim_start_matches('/'))),
        _ => false,
    }
}

struct RustLocation<'a> {
    crate_root: &'a str,
    // None outside `src/`: integration tests and examples are not imported by name.
    crate_name: Option<String>,
    module: Vec<&'a str>,
}

impl<'a> RustLocation<'a> {
    fn of(path: &'a str, repo: &str) -> Self {
        let src_start = path
            .rfind("/src/")
            .map(|index| index + 1)
            .or_else(|| path.starts_with("src/").then_some(0));

        let (crate_root, relative, crate_name) = match src_start {
            Some(start) => {
                let root = &path[..start];
                let directory = root.trim_end_matches('/').rsplit('/').next().unwrap_or("");
                let name = if directory.is_empty() {
                    repo
                } else {
                    directory
                };
                (root, &path[start + 4..], Some(name.replace('-', "_")))
            }
            None => {
                let split = path.rfind('/').map_or(0, |index| index + 1);
                (&path[..split], &path[split..], None)
            }
        };

        let mut module: Vec<&str> = relative.trim_end_matches(".rs").split('/').collect();
        if matches!(module.last(), Some(&"mod" | &"lib" | &"main")) {
            module.pop();
        }
        RustLocation {
            crate_root,
            crate_name,
            module,
        }
    }
}

fn declares_module(symbol: &Symbol, from: &RustLocation, target: &RustLocation) -> bool {
    // `mod name;` fits on one line; an inline `mod name { ... }` is not another file.
    symbol.kind == "module"
        && symbol.parent.is_none()
        && symbol.end_line == Some(symbol.line)
        && from.crate_root == target.crate_root
        && from
            .module
            .iter()
            .copied()
            .chain([symbol.name.as_str()])
            .eq(target.module.iter().copied())
}

fn use_paths(import: &Import) -> Vec<Vec<String>> {
    let base: Vec<String> = import
        .source
        .split("::")
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect();
    let mut paths = Vec::new();
    match &import.names {
        Some(ImportNames::List(names)) => {
            for name in names {
                expand_use_item(&base, name, &mut paths);
            }
        }
        _ => paths.push(base),
    }
    paths
}

fn expand_use_item(prefix: &[String], item: &str, paths: &mut Vec<Vec<String>>) {
    let mut path = prefix.to_vec();
    if let Some(open) = item.find('{') {
        path.extend(
            item[..open]
                .split("::")
                .filter(|segment| !segment.is_empty())
                .map(str::to_string),
        );
        let inner = item[open + 1..].trim_end().trim_end_matches('}');
        for nested in split_top_level(inner) {
            expand_use_item(&path, &nested, paths);
        }
        return;
    }
    if !matches!(item, "self" | "*") {
        path.extend(item.split("::").map(str::to_string));
    }
    paths.push(path);
}

fn rust_path_reaches(path: &[String], from: &RustLocation, target: &RustLocation) -> bool {
    let Some(first) = path.first() else {
        return false;
    };
    let same_crate = from.crate_root == target.crate_root;
    let local = |segments: &[String]| -> Vec<String> {
        from.module
            .iter()
            .map(|segment| segment.to_string())
            .chain(segments.iter().cloned())
            .collect()
    };

    let absolute: Vec<String> = match first.as_str() {
        "crate" if same_crate => path[1..].to_vec(),
        "self" if same_crate => local(&path[1..]),
        "super" if same_crate => {
            let climbs = path.iter().take_while(|s| *s == "super").count();
            let Some(kept) = from.module.len().checked_sub(climbs) else {
                return false;
            };
            from.module[..kept]
                .iter()
                .map(|segment| segment.to_string())
                .chain(path[climbs..].iter().cloned())
                .collect()
        }
        "crate" | "self" | "super" => return false,
        name if target.crate_name.as_deref() == Some(name) => path[1..].to_vec(),
        _ if same_crate => local(path),
        _ => return false,
    };

    // `self::` and bare paths only reach downward, so they can't name the importer or its parents.
    let is_downward = !matches!(first.as_str(), "crate" | "super")
        && target.crate_name.as_deref() != Some(first.as_str());
    if is_downward && target.module.len() <= from.module.len() {
        return false;
    }

    // A crate root holds items, not submodules, so a longer path points past it.
    if target.module.is_empty() {
        return absolute.len() <= 1;
    }
    absolute.len() >= target.module.len()
        && target
            .module
            .iter()
            .zip(&absolute)
            .all(|(wanted, segment)| *wanted == segment)
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
    use super::{
        RustLocation, declares_module, ends_with_path, module_key, points_at, rust_path_reaches,
        use_paths,
    };
    use crate::model::{Import, ImportNames, Symbol};

    fn imports(importer: &str, lang: &str, source: &str, target_path: &str) -> bool {
        let import = Import {
            source: source.to_string(),
            names: None,
        };
        points_at(importer, lang, &import, &module_key(target_path))
    }

    fn import_with_names(source: &str, names: &[&str]) -> Import {
        Import {
            source: source.to_string(),
            names: Some(ImportNames::List(
                names.iter().map(|name| name.to_string()).collect(),
            )),
        }
    }

    fn rust_imports(importer: &str, source: &str, names: &[&str], target: &str) -> bool {
        let from = RustLocation::of(importer, "my-repo");
        let target = RustLocation::of(target, "my-repo");
        use_paths(&import_with_names(source, names))
            .iter()
            .any(|path| rust_path_reaches(path, &from, &target))
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
    fn python_modules_named_in_the_import_list_resolve() {
        let sibling = import_with_names(".", &["mod"]);
        let target = module_key("src/pkg/mod.py");
        assert!(points_at("src/pkg/other.py", "python", &sibling, &target));
        assert!(!points_at("src/app/other.py", "python", &sibling, &target));

        let absolute = import_with_names("pkg", &["mod", "other"]);
        assert!(points_at("src/app.py", "python", &absolute, &target));
    }

    #[test]
    fn rust_crate_paths_resolve() {
        let target = "src/model.rs";
        assert!(rust_imports(
            "src/main.rs",
            "crate::model",
            &["Index"],
            target
        ));
        assert!(rust_imports("src/a/b.rs", "crate", &["model"], target));
        assert!(rust_imports(
            "src/a.rs",
            "crate",
            &["other", "model::{Index, Symbol}"],
            target
        ));
        assert!(!rust_imports("src/a.rs", "crate::modeling", &["X"], target));
        assert!(!rust_imports(
            "src/a.rs",
            "crate::other",
            &["model"],
            target
        ));
    }

    #[test]
    fn rust_self_super_and_relative_paths_resolve() {
        assert!(rust_imports(
            "src/extract/python.rs",
            "super",
            &["Extracted"],
            "src/extract/mod.rs"
        ));
        assert!(rust_imports(
            "src/a/b/c.rs",
            "super::super::d",
            &["X"],
            "src/a/d.rs"
        ));
        assert!(rust_imports(
            "src/extract/mod.rs",
            "self::python",
            &["run"],
            "src/extract/python.rs"
        ));
        assert!(rust_imports(
            "src/main.rs",
            "index",
            &["run"],
            "src/index.rs"
        ));
        assert!(!rust_imports(
            "src/a.rs",
            "super::super",
            &["X"],
            "src/x.rs"
        ));
    }

    #[test]
    fn rust_external_crates_do_not_match_enclosing_modules() {
        let parent = "src/extract/mod.rs";
        let child = "src/extract/python.rs";
        assert!(!rust_imports(
            child,
            "std::collections",
            &["HashSet"],
            parent
        ));
        assert!(!rust_imports(child, "tree_sitter", &["Node"], parent));
        assert!(!rust_imports(child, "self::inner", &["X"], parent));
        assert!(!rust_imports(
            "src/a.rs",
            "anyhow",
            &["Result"],
            "src/lib.rs"
        ));
        assert!(!rust_imports("src/a.rs", "", &["anyhow"], "src/lib.rs"));
    }

    #[test]
    fn rust_nested_use_groups_expand() {
        assert!(rust_imports(
            "src/a.rs",
            "crate::x",
            &["y::{z, w::{v}}"],
            "src/x/y/w/v.rs"
        ));
        assert!(rust_imports(
            "src/a.rs",
            "crate::x",
            &["y::{self}"],
            "src/x/y.rs"
        ));
    }

    #[test]
    fn rust_paths_cross_workspace_crates_by_name() {
        let target = "rust/crates/forge-core/src/jsnum.rs";
        assert!(rust_imports(
            "rust/crates/forge-data/src/a.rs",
            "forge_core::jsnum",
            &["js_round"],
            target
        ));
        assert!(rust_imports(
            "rust/crates/forge-core/tests/jsnum.rs",
            "forge_core",
            &["jsnum"],
            target
        ));
        assert!(!rust_imports(
            "rust/crates/forge-data/src/a.rs",
            "crate::jsnum",
            &["x"],
            target
        ));
        assert!(!rust_imports(
            "rust/crates/forge-data/src/a.rs",
            "other_crate::jsnum",
            &["x"],
            target
        ));
    }

    #[test]
    fn rust_crate_roots_are_not_matched_by_every_path() {
        assert!(rust_imports("src/a.rs", "crate", &["Item"], "src/lib.rs"));
        assert!(!rust_imports(
            "src/a.rs",
            "crate::model",
            &["Index"],
            "src/lib.rs"
        ));
    }

    #[test]
    fn rust_mod_declarations_count_but_inline_modules_do_not() {
        let module = |name: &str, end_line: u32| Symbol {
            name: name.to_string(),
            kind: "module".to_string(),
            line: 1,
            end_line: Some(end_line),
            exported: false,
            is_async: false,
            parent: None,
            decorators: None,
        };
        let main = RustLocation::of("src/main.rs", "repo");
        let target = RustLocation::of("src/lookup.rs", "repo");
        assert!(declares_module(&module("lookup", 1), &main, &target));
        assert!(!declares_module(&module("lookup", 9), &main, &target));
        assert!(!declares_module(&module("other", 1), &main, &target));

        let sibling = RustLocation::of("src/a.rs", "repo");
        assert!(!declares_module(&module("lookup", 1), &sibling, &target));
    }
}
