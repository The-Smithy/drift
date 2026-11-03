use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use ignore::WalkBuilder;
use rayon::prelude::*;

use crate::extract::{self, Extracted, UNKNOWN_FRAMEWORK};
use crate::model::{FileEntry, Index, SCHEMA_VERSION, SymbolLocation};

const DEFAULT_IGNORE_DIRS: &[&str] = &[
    "node_modules",
    "__pycache__",
    "venv",
    "env",
    "dist",
    "build",
    "out",
    "target",
    "coverage",
    "vendor",
    "deps",
    "_build",
    "graphify-out",
];

const DEFAULT_IGNORE_FILES: &[&str] = &[
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "Cargo.lock",
    "poetry.lock",
    "Pipfile.lock",
    ".DS_Store",
    "Thumbs.db",
];

const MINIFIED_SUFFIXES: &[&str] = &[".min.js", ".min.mjs", ".min.cjs"];

const MAX_FILE_SIZE: u64 = 512 * 1024;

pub const INDEX_FILE: &str = ".drift-index.json";

// canonicalize() yields \\?\C:\... on Windows, which is noise in output and in the index.
pub fn resolve_root(path: &Path) -> Result<PathBuf> {
    let resolved = path
        .canonicalize()
        .with_context(|| format!("{} is not accessible", path.display()))?;
    anyhow::ensure!(resolved.is_dir(), "{} is not a directory", path.display());
    let display = resolved.to_string_lossy();
    Ok(match display.strip_prefix(r"\\?\") {
        Some(stripped) => PathBuf::from(stripped),
        None => resolved,
    })
}

pub fn index_repo(root: &Path, extra_dirs: &[String], extra_files: &[String]) -> Result<Index> {
    let paths = walk(root, extra_dirs, extra_files);

    let extracted: Vec<(String, Extracted)> = paths
        .par_iter()
        .filter_map(|path| {
            let extension = lowercase_extension(path)?;
            let content = decode(&fs::read(path).ok()?);
            let source = content.strip_prefix('\u{feff}').unwrap_or(&content);
            let mut result = extract::extract(&extension, source)?;

            if let Some(tests) = &mut result.tests
                && tests.framework == UNKNOWN_FRAMEWORK
            {
                tests.framework = framework_from_package_json(path, root).to_string();
            }
            let relative = path.strip_prefix(root).ok()?;
            Some((relative.to_string_lossy().replace('\\', "/"), result))
        })
        .collect();

    let mut files = BTreeMap::new();
    let mut tests = BTreeMap::new();
    let mut languages = BTreeSet::new();
    for (path, result) in extracted {
        languages.insert(result.lang.to_string());
        if let Some(file_tests) = result.tests {
            tests.insert(path.clone(), file_tests);
        }
        files.insert(
            path,
            FileEntry {
                lang: result.lang.to_string(),
                lines: result.lines,
                symbols: result.symbols,
                imports: result.imports,
            },
        );
    }

    let mut symbol_index: BTreeMap<String, Vec<SymbolLocation>> = BTreeMap::new();
    for (path, file) in &files {
        for symbol in &file.symbols {
            symbol_index
                .entry(symbol.name.clone())
                .or_default()
                .push(SymbolLocation {
                    file: path.clone(),
                    line: symbol.line,
                    end_line: symbol.end_line,
                    kind: symbol.kind.clone(),
                    exported: symbol.exported,
                });
        }
    }

    Ok(Index {
        version: SCHEMA_VERSION.to_string(),
        repo: root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        root: root.to_string_lossy().into_owned(),
        generated_at: humantime::format_rfc3339_seconds(SystemTime::now()).to_string(),
        languages: languages.into_iter().collect(),
        files,
        tests,
        symbol_index,
    })
}

pub fn write_index(index: &Index, output: &Path, compact: bool) -> Result<()> {
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let file = File::create(output).with_context(|| format!("creating {}", output.display()))?;
    let mut writer = BufWriter::new(file);
    if compact {
        serde_json::to_writer(&mut writer, &index.to_compact())?;
    } else {
        serde_json::to_writer_pretty(&mut writer, index)?;
    }
    writer.flush()?;
    Ok(())
}

pub fn read_index(path: &Path) -> Result<Index> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("{} not found. Run 'drift index' first.", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("{} is not a valid index", path.display()))
}

// PowerShell's `>` redirection writes UTF-16, so generated files can arrive that way on Windows.
fn decode(bytes: &[u8]) -> String {
    let utf16 = |to_unit: fn([u8; 2]) -> u16| -> String {
        let units = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| to_unit([pair[0], pair[1]]));
        char::decode_utf16(units)
            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect()
    };
    match bytes {
        [0xFF, 0xFE, ..] => utf16(u16::from_le_bytes),
        [0xFE, 0xFF, ..] => utf16(u16::from_be_bytes),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

fn lowercase_extension(path: &Path) -> Option<String> {
    Some(path.extension()?.to_str()?.to_ascii_lowercase())
}

fn walk(root: &Path, extra_dirs: &[String], extra_files: &[String]) -> Vec<PathBuf> {
    let ignored_dirs: HashSet<String> = DEFAULT_IGNORE_DIRS
        .iter()
        .map(|d| d.to_string())
        .chain(extra_dirs.iter().cloned())
        .collect();
    let ignored_files: HashSet<String> = DEFAULT_IGNORE_FILES
        .iter()
        .map(|f| f.to_string())
        .chain(extra_files.iter().cloned())
        .collect();

    // Not WalkBuilder::max_filesize: setting it makes the walker skip filter_entry for files.
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .parents(false)
        .git_global(false)
        .require_git(false)
        .filter_entry(move |entry| {
            let name = entry.file_name().to_string_lossy();
            if entry.file_type().is_some_and(|t| t.is_dir()) {
                entry.depth() == 0
                    || (!name.starts_with('.') && !ignored_dirs.contains(name.as_ref()))
            } else {
                !ignored_files.contains(name.as_ref())
                    && !MINIFIED_SUFFIXES.iter().any(|s| name.ends_with(s))
            }
        })
        .build();

    walker
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|t| t.is_file()))
        .filter(|entry| {
            lowercase_extension(entry.path()).is_some_and(|e| extract::is_supported(&e))
        })
        .filter(|entry| entry.metadata().is_ok_and(|m| m.len() <= MAX_FILE_SIZE))
        .map(|entry| entry.into_path())
        .collect()
}

// Test globals need no import, so the nearest package.json is the next best signal.
fn framework_from_package_json(file: &Path, root: &Path) -> &'static str {
    for directory in file.ancestors().skip(1) {
        if let Ok(manifest) = fs::read_to_string(directory.join("package.json")) {
            for (dependency, framework) in [
                ("\"vitest\"", "vitest"),
                ("\"jest\"", "jest"),
                ("\"@playwright/test\"", "playwright"),
                ("\"mocha\"", "mocha"),
                ("\"cypress\"", "cypress"),
            ] {
                if manifest.contains(dependency) {
                    return framework;
                }
            }
        }
        if directory == root {
            break;
        }
    }
    UNKNOWN_FRAMEWORK
}
