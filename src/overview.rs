use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::process::Command;

use anyhow::{Context, Result, ensure};

use crate::imports::{RustLocation, module_key, resolve_source, use_paths};
use crate::lookup::{Answer, View, counted, innermost_symbol, path_tier};
use crate::model::{ImportNames, Index, Symbol};

const MAP_FOLDERS: usize = 12;
const MAP_ENTRY_POINTS: usize = 8;
const MAP_IMPORTED: usize = 10;
const MAP_NAMES: usize = 4;

// Maps each file to the files that import it, resolving JS, TS, Python and Rust paths.
pub fn import_graph(index: &Index) -> BTreeMap<String, BTreeSet<String>> {
    let mut by_key: HashMap<String, &str> = HashMap::new();
    let mut rust_modules: HashMap<(&str, String), &str> = HashMap::new();
    let mut crates: HashMap<String, &str> = HashMap::new();

    for (path, file) in &index.files {
        if file.lang == "rust" {
            let location = RustLocation::of(path, &index.repo);
            if let Some(name) = location.crate_name {
                crates.entry(name).or_insert(location.crate_root);
            }
            if !location.module.is_empty() {
                rust_modules.insert((location.crate_root, location.module.join("/")), path);
            }
        } else {
            by_key.insert(module_key(path), path);
        }
    }

    let mut graph: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (path, file) in &index.files {
        let mut targets: BTreeSet<&str> = BTreeSet::new();
        if file.lang == "rust" {
            let from = RustLocation::of(path, &index.repo);
            for import in &file.imports {
                for use_path in use_paths(import) {
                    if let Some(target) = rust_target(&use_path, &from, &rust_modules, &crates) {
                        targets.insert(target);
                    }
                }
            }
        } else {
            for import in &file.imports {
                let Some(resolved) = resolve_source(path, &file.lang, &import.source) else {
                    continue;
                };
                let mut candidates = vec![resolved.clone()];
                if let (Some(ImportNames::List(names)), "python") =
                    (&import.names, file.lang.as_str())
                {
                    candidates.extend(names.iter().map(|name| {
                        format!("{resolved}/{name}")
                            .trim_start_matches('/')
                            .to_string()
                    }));
                }
                for candidate in candidates {
                    let key = module_key(&candidate);
                    let target = by_key
                        .get(key.as_str())
                        .or_else(|| by_key.get(format!("src/{key}").as_str()));
                    if let Some(target) = target {
                        targets.insert(target);
                    }
                }
            }
        }
        for target in targets {
            if target != path {
                graph
                    .entry(target.to_string())
                    .or_default()
                    .insert(path.clone());
            }
        }
    }
    graph
}

fn rust_target<'a>(
    path: &[String],
    from: &RustLocation,
    modules: &HashMap<(&str, String), &'a str>,
    crates: &HashMap<String, &str>,
) -> Option<&'a str> {
    let first = path.first()?;
    let own_module = || from.module.iter().map(|s| s.to_string());
    let (root, absolute): (&str, Vec<String>) = match first.as_str() {
        "crate" => (from.crate_root, path[1..].to_vec()),
        "self" => (
            from.crate_root,
            own_module().chain(path[1..].iter().cloned()).collect(),
        ),
        "super" => {
            let climbs = path.iter().take_while(|s| *s == "super").count();
            let kept = from.module.len().checked_sub(climbs)?;
            (
                from.crate_root,
                from.module[..kept]
                    .iter()
                    .map(|s| s.to_string())
                    .chain(path[climbs..].iter().cloned())
                    .collect(),
            )
        }
        name => match crates.get(name) {
            Some(root) if *root != from.crate_root => (*root, path[1..].to_vec()),
            _ => (
                from.crate_root,
                own_module().chain(path.iter().cloned()).collect(),
            ),
        },
    };
    (1..=absolute.len())
        .rev()
        .find_map(|length| modules.get(&(root, absolute[..length].join("/"))).copied())
}

fn is_entry_point(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    let stem = file.split('.').next().unwrap_or(file);
    let depth = path.matches('/').count();
    let is_document =
        file.ends_with(".md") || file.ends_with(".mdx") || file.ends_with(".markdown");
    depth <= 2
        && !is_document
        && matches!(
            stem,
            "main" | "cli" | "server" | "app" | "__main__" | "manage" | "index" | "lib"
        )
}

pub fn map(index: &Index, view: &View) -> Answer {
    let in_view: Vec<(&String, &crate::model::FileEntry)> = index
        .files
        .iter()
        .filter(|(path, _)| view.includes(path))
        .collect();
    if in_view.is_empty() {
        return Answer {
            lines: vec!["No indexed files here".to_string()],
            found: false,
        };
    }

    let total_lines: u64 = in_view.iter().map(|(_, f)| u64::from(f.lines)).sum();
    let symbols: usize = in_view.iter().map(|(_, f)| f.symbols.len()).sum();
    let mut languages: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, file) in &in_view {
        *languages.entry(&file.lang).or_default() += 1;
    }
    let mut languages: Vec<_> = languages.into_iter().collect();
    languages.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    let languages = languages
        .iter()
        .map(|(name, count)| format!("{name} {count}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut lines = vec![
        format!(
            "{}  {}, {total_lines} lines, {symbols} symbols",
            index.repo,
            counted(in_view.len(), "file")
        ),
        format!("Languages: {languages}"),
    ];

    let mut folders: BTreeMap<String, usize> = BTreeMap::new();
    for (path, _) in in_view.iter().filter(|(path, _)| path_tier(path) == 0) {
        let parts: Vec<&str> = path.split('/').collect();
        let folder = match parts.len() {
            1 => ".".to_string(),
            2 => parts[0].to_string(),
            _ => parts[..2].join("/"),
        };
        *folders.entry(folder).or_default() += 1;
    }
    let mut folders: Vec<_> = folders.into_iter().collect();
    folders.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    if !folders.is_empty() {
        lines.push("Folders:".to_string());
        lines.extend(
            folders
                .iter()
                .take(MAP_FOLDERS)
                .map(|(folder, count)| format!("  {folder}  {}", counted(*count, "file"))),
        );
        if folders.len() > MAP_FOLDERS {
            lines.push(format!("  ... {} more", folders.len() - MAP_FOLDERS));
        }
    }

    let entry_points: Vec<&str> = in_view
        .iter()
        .map(|(path, _)| path.as_str())
        .filter(|path| path_tier(path) == 0 && is_entry_point(path))
        .take(MAP_ENTRY_POINTS)
        .collect();
    if !entry_points.is_empty() {
        lines.push("Entry points:".to_string());
        lines.extend(entry_points.iter().map(|path| format!("  {path}")));
    }

    let graph = import_graph(index);
    let mut imported: Vec<(&String, usize)> = graph
        .iter()
        .filter(|(path, _)| path_tier(path) == 0 && view.includes(path))
        .map(|(path, importers)| (path, importers.len()))
        .collect();
    imported.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    if !imported.is_empty() {
        lines.push("Most imported:".to_string());
        for (path, count) in imported.iter().take(MAP_IMPORTED) {
            let names: Vec<&str> = index.files[path.as_str()]
                .symbols
                .iter()
                .filter(|symbol| symbol.exported && symbol.parent.is_none())
                .take(MAP_NAMES)
                .map(|symbol| symbol.name.as_str())
                .collect();
            let importers = counted(*count, "importer");
            lines.push(if names.is_empty() {
                format!("  {path}  {importers}")
            } else {
                format!("  {path}  {importers}: {}", names.join(", "))
            });
        }
    }

    let test_files = index
        .tests
        .keys()
        .filter(|path| view.includes(path))
        .count();
    if test_files > 0 {
        let frameworks: BTreeSet<&str> = index
            .tests
            .values()
            .map(|file| file.framework.as_str())
            .collect();
        lines.push(format!(
            "Tests: {} ({})",
            counted(test_files, "file"),
            frameworks.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    Answer { lines, found: true }
}

fn run_git(root: &str, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(["-c", "core.quotepath=off"])
        .args(args)
        .current_dir(root)
        .output()
        .context("Couldn't run git")?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

struct FileChange {
    path: String,
    deleted: bool,
    lines: Vec<u32>,
}

fn parse_diff(diff: &str) -> Vec<FileChange> {
    let mut changes: Vec<FileChange> = Vec::new();
    let mut old_path = String::new();
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("--- ") {
            old_path = path.strip_prefix("a/").unwrap_or(path).to_string();
        } else if let Some(path) = line.strip_prefix("+++ ") {
            let deleted = path == "/dev/null";
            let path = if deleted {
                old_path.clone()
            } else {
                path.strip_prefix("b/").unwrap_or(path).to_string()
            };
            changes.push(FileChange {
                path,
                deleted,
                lines: Vec::new(),
            });
        } else if line.starts_with("@@ ")
            && let Some(change) = changes.last_mut()
            && let Some(added) = line.split(' ').find(|part| part.starts_with('+'))
        {
            let mut numbers = added[1..].split(',');
            let start: u32 = numbers.next().and_then(|n| n.parse().ok()).unwrap_or(0);
            let count: u32 = numbers.next().and_then(|n| n.parse().ok()).unwrap_or(1);
            if count == 0 {
                // A pure deletion: the line before the gap is the closest thing to where it happened.
                change.lines.push(start.max(1));
            } else {
                change.lines.extend(start..start + count);
            }
        }
    }
    changes
}

pub fn changes(index: &Index, reference: Option<&str>, view: &View) -> Result<Answer> {
    let reference = reference.unwrap_or("HEAD");
    // A leading dash would be read by git as an option rather than a ref.
    ensure!(
        !reference.starts_with('-'),
        "'{reference}' is not a git ref"
    );
    let diff = run_git(
        &index.root,
        &[
            "diff",
            "--unified=0",
            "--no-color",
            "--no-ext-diff",
            reference,
            "--",
        ],
    )?;
    let mut changes = parse_diff(&diff);
    let untracked = run_git(&index.root, &["ls-files", "--others", "--exclude-standard"])?;
    let new_files: Vec<&str> = untracked.lines().collect();
    changes.retain(|change| view.includes(&change.path));

    let mut lines = Vec::new();
    for change in &changes {
        let Some(entry) = index.files.get(&change.path) else {
            let note = if change.deleted {
                "deleted"
            } else {
                "not indexed"
            };
            lines.push(format!("{}  {note}", change.path));
            continue;
        };
        lines.push(change.path.clone());
        let mut touched: Vec<&Symbol> = Vec::new();
        let mut outside = 0;
        for line in &change.lines {
            match innermost_symbol(&entry.symbols, *line) {
                Some(symbol) => {
                    if !touched.iter().any(|seen| std::ptr::eq(*seen, symbol)) {
                        touched.push(symbol);
                    }
                }
                None => outside += 1,
            }
        }
        for symbol in touched {
            let range = match symbol.end_line {
                Some(end) if end != symbol.line => format!("{}-{end}", symbol.line),
                _ => symbol.line.to_string(),
            };
            let exported = if symbol.exported { " (exported)" } else { "" };
            lines.push(format!(
                "  {} {}  {range}{exported}",
                symbol.kind, symbol.name
            ));
        }
        if outside > 0 {
            lines.push(format!("  {outside} lines outside any definition"));
        }
    }
    for path in new_files.iter().filter(|path| view.includes(path)) {
        lines.push(format!("{path}  new, untracked"));
    }
    if lines.is_empty() {
        return Ok(Answer {
            lines: vec![format!("No changes since {reference}")],
            found: true,
        });
    }
    Ok(Answer { lines, found: true })
}

#[cfg(test)]
mod tests {
    use super::{is_entry_point, parse_diff};

    #[test]
    fn diffs_map_to_new_side_line_numbers() {
        let diff = "diff --git a/a.ts b/a.ts\n\
                    --- a/a.ts\n\
                    +++ b/a.ts\n\
                    @@ -1,0 +2,3 @@ fn\n\
                    +x\n\
                    @@ -9 +12 @@\n\
                    @@ -20,2 +24,0 @@\n\
                    diff --git a/gone.ts b/gone.ts\n\
                    --- a/gone.ts\n\
                    +++ /dev/null\n\
                    @@ -1,2 +0,0 @@\n";
        let changes = parse_diff(diff);
        assert_eq!(changes[0].path, "a.ts");
        assert_eq!(changes[0].lines, vec![2, 3, 4, 12, 24]);
        assert_eq!(changes[1].path, "gone.ts");
        assert!(changes[1].deleted);
    }

    #[test]
    fn entry_points_are_shallow_conventional_files() {
        assert!(is_entry_point("src/main.rs"));
        assert!(is_entry_point("src/index.ts"));
        assert!(!is_entry_point("src/a/b/c/index.ts"));
        assert!(!is_entry_point("src/lookup.rs"));
    }
}
