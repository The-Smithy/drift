use crate::extract::split_top_level;
use crate::model::{Import, ImportNames, Symbol};

const SOURCE_EXTENSIONS: &[&str] = &[
    ".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs", ".py", ".rs", ".lua", ".gd",
];

pub fn ends_with_path(path: &str, suffix: &str) -> bool {
    !suffix.is_empty()
        && path
            .strip_suffix(suffix)
            .is_some_and(|rest| rest.is_empty() || rest.ends_with('/'))
}

// `src/lib/auth.ts` and `src/lib/auth/index.ts` are both imported as `src/lib/auth`.
pub fn module_key(path: &str) -> String {
    let lowered = path.to_lowercase();
    let without_extension = SOURCE_EXTENSIONS
        .iter()
        .find_map(|extension| lowered.strip_suffix(extension))
        .unwrap_or(&lowered);
    let key = without_extension
        .strip_suffix("/index")
        .or_else(|| without_extension.strip_suffix("/__init__"))
        .or_else(|| {
            lowered
                .ends_with(".lua")
                .then(|| without_extension.strip_suffix("/init"))
                .flatten()
        })
        .unwrap_or(without_extension);
    key.trim_end_matches('/').to_string()
}

pub fn points_at(importer: &str, lang: &str, import: &Import, target: &str) -> bool {
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

pub struct RustLocation<'a> {
    pub crate_root: &'a str,
    // None outside `src/`: integration tests and examples are not imported by name.
    pub crate_name: Option<String>,
    pub module: Vec<&'a str>,
}

impl<'a> RustLocation<'a> {
    pub fn of(path: &'a str, repo: &str) -> Self {
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

pub fn declares_module(symbol: &Symbol, from: &RustLocation, target: &RustLocation) -> bool {
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

pub fn use_paths(import: &Import) -> Vec<Vec<String>> {
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

pub fn rust_path_reaches(path: &[String], from: &RustLocation, target: &RustLocation) -> bool {
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

pub fn resolve_source(importer: &str, lang: &str, source: &str) -> Option<String> {
    if lang == "python" {
        let dots = source.chars().take_while(|c| *c == '.').count();
        let module = source[dots..].replace('.', "/");
        if dots == 0 {
            return Some(module);
        }
        let climb = "../".repeat(dots - 1);
        return join_relative(importer, &format!("./{climb}{module}"));
    }
    match lang {
        // C# imports name a namespace, which says nothing about the file.
        "rust" | "csharp" => return None,
        "lua" => return Some(source.replace('.', "/").to_lowercase()),
        // Include paths are searched for, so this is matched by its trailing path.
        "c" | "cpp" if !source.starts_with('.') => return Some(source.to_lowercase()),
        "gdscript" => {
            if let Some(path) = source.strip_prefix("res://") {
                return Some(path.to_string());
            }
        }
        _ => {}
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
            signature: None,
            doc: None,
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
