mod extract;
mod index;
mod lookup;
mod model;

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Result, ensure};
use clap::{Parser, Subcommand};

use index::{INDEX_FILE, index_repo, read_index, resolve_root, write_index};
use model::Index;

const SKILL: &str = include_str!("skill.md");

// musl's allocator contends badly across threads, which made parallel indexing about 3x slower.
#[cfg(target_env = "musl")]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Parser)]
#[command(
    name = "drift",
    version,
    about = "Index a codebase for fast agent-searchable lookups.",
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate .drift-index.json for a repo
    Index {
        /// Path to repo root
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Output file path (default: <repo>/.drift-index.json)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Extra directories to ignore
        #[arg(long, num_args = 0..)]
        ignore_dirs: Vec<String>,
        /// Extra files to ignore
        #[arg(long, num_args = 0..)]
        ignore_files: Vec<String>,
        /// Compact output: strip nulls, only exported symbols in symbol_index
        #[arg(long)]
        compact: bool,
    },
    /// Look up where a symbol is defined
    Query {
        /// Symbol name; falls back to a case-insensitive substring match
        symbol: String,
        /// Path to index file
        #[arg(long, default_value = INDEX_FILE)]
        index: PathBuf,
    },
    /// List a file's symbols and imports without reading it
    Outline {
        /// File path, or a trailing part of one such as lib/auth.ts
        file: String,
        /// Path to index file
        #[arg(long, default_value = INDEX_FILE)]
        index: PathBuf,
    },
    /// List test files, or find tests by path, suite or case name
    Tests {
        /// Case-insensitive text to look for; omit to summarize every test file
        pattern: Option<String>,
        /// Path to index file
        #[arg(long, default_value = INDEX_FILE)]
        index: PathBuf,
    },
    /// Find the files that import a module
    Importers {
        /// File path such as src/lib/auth.ts, or text to find in import sources
        module: String,
        /// Path to index file
        #[arg(long, default_value = INDEX_FILE)]
        index: PathBuf,
    },
    /// Show index statistics
    Stats {
        /// Path to index file
        #[arg(long, default_value = INDEX_FILE)]
        index: PathBuf,
    },
    /// Index every git repo directly under a parent directory
    Batch {
        /// Parent directory containing repos
        parent: PathBuf,
        /// Compact output
        #[arg(long)]
        compact: bool,
        /// Also install the Claude Code skill in each repo
        #[arg(long)]
        install: bool,
    },
    /// Install the Claude Code skill into a repo
    Install {
        /// Path to repo root
        #[arg(default_value = ".")]
        path: PathBuf,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Index {
            path,
            output,
            ignore_dirs,
            ignore_files,
            compact,
        } => cmd_index(&path, output, &ignore_dirs, &ignore_files, compact),
        Command::Query { symbol, index } => lookup::query(&symbol, &index),
        Command::Outline { file, index } => lookup::outline(&file, &index),
        Command::Tests { pattern, index } => lookup::tests(pattern.as_deref(), &index),
        Command::Importers { module, index } => lookup::importers(&module, &index),
        Command::Stats { index } => cmd_stats(&index),
        Command::Batch {
            parent,
            compact,
            install,
        } => cmd_batch(&parent, compact, install),
        Command::Install { path } => {
            let skill = install_skill(&resolve_root(&path)?)?;
            println!("Installed skill at {}", skill.display());
            println!(
                "Run 'drift index' to generate the index, then agents will use it automatically."
            );
            Ok(())
        }
    }
}

fn cmd_index(
    path: &Path,
    output: Option<PathBuf>,
    ignore_dirs: &[String],
    ignore_files: &[String],
    compact: bool,
) -> Result<()> {
    let root = resolve_root(path)?;
    let output = output.unwrap_or_else(|| root.join(INDEX_FILE));

    let start = Instant::now();
    let index = index_repo(&root, ignore_dirs, ignore_files)?;
    let elapsed = start.elapsed();
    write_index(&index, &output, compact)?;

    println!(
        "Indexed {} files, {} symbols, {} tests",
        index.files.len(),
        index.symbol_count(),
        index.test_count()
    );
    println!("Languages: {}", index.languages.join(", "));
    println!("Output: {} ({:.1} KB)", output.display(), size_kb(&output));
    println!("Time: {:.2}s", elapsed.as_secs_f64());
    Ok(())
}

fn cmd_stats(index_path: &Path) -> Result<()> {
    let index = read_index(index_path)?;

    let total_lines: u64 = index.files.values().map(|f| u64::from(f.lines)).sum();
    let unique: HashSet<&str> = index
        .files
        .values()
        .flat_map(|f| &f.symbols)
        .map(|s| s.name.as_str())
        .collect();

    let mut languages: BTreeMap<&str, usize> = BTreeMap::new();
    for file in index.files.values() {
        *languages.entry(&file.lang).or_default() += 1;
    }
    let mut languages: Vec<_> = languages.into_iter().collect();
    languages.sort_by_key(|language| std::cmp::Reverse(language.1));

    println!("Repo:       {}", index.repo);
    println!("Generated:  {}", index.generated_at);
    println!("Files:      {}", index.files.len());
    println!("Lines:      {total_lines}");
    println!(
        "Symbols:    {} ({} unique)",
        index.symbol_count(),
        unique.len()
    );
    println!(
        "Tests:      {} across {} files",
        index.test_count(),
        index.tests.len()
    );
    println!("Languages:");
    for (language, count) in languages {
        println!("  {language:12} {count} files");
    }
    Ok(())
}

fn cmd_batch(parent: &Path, compact: bool, install: bool) -> Result<()> {
    let parent = resolve_root(parent)?;
    let mut repos: Vec<PathBuf> = fs::read_dir(&parent)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && path.join(".git").exists())
        .collect();
    repos.sort();
    ensure!(
        !repos.is_empty(),
        "No git repos found under {}",
        parent.display()
    );

    println!("Found {} repos under {}\n", repos.len(), parent.display());

    let batch_start = Instant::now();
    let (mut total_files, mut total_symbols, mut total_tests) = (0, 0, 0);
    for repo in &repos {
        let name = repo.file_name().unwrap_or_default().to_string_lossy();
        match index_one(repo, compact, install) {
            Ok((index, output, elapsed)) => {
                total_files += index.files.len();
                total_symbols += index.symbol_count();
                total_tests += index.test_count();
                let languages = if index.languages.is_empty() {
                    "none".to_string()
                } else {
                    index.languages.join(", ")
                };
                println!(
                    "  {:30}  {:4} files  {:5} symbols  {:4} tests  {:7.1} KB  ({:.2}s)  [{}]",
                    name,
                    index.files.len(),
                    index.symbol_count(),
                    index.test_count(),
                    size_kb(&output),
                    elapsed,
                    languages
                );
            }
            Err(error) => eprintln!("  {name:30}  ERROR: {error:#}"),
        }
    }

    println!(
        "\nTotal: {} files, {} symbols, {} tests across {} repos in {:.2}s",
        total_files,
        total_symbols,
        total_tests,
        repos.len(),
        batch_start.elapsed().as_secs_f64()
    );
    Ok(())
}

fn index_one(repo: &Path, compact: bool, install: bool) -> Result<(Index, PathBuf, f64)> {
    let start = Instant::now();
    let index = index_repo(repo, &[], &[])?;
    let output = repo.join(INDEX_FILE);
    write_index(&index, &output, compact)?;
    let elapsed = start.elapsed().as_secs_f64();
    if install && !index.files.is_empty() {
        install_skill(repo)?;
    }
    Ok((index, output, elapsed))
}

fn install_skill(root: &Path) -> Result<PathBuf> {
    let directory = root.join(".claude").join("skills").join("drift");
    fs::create_dir_all(&directory)?;
    let skill = directory.join("SKILL.md");
    fs::write(&skill, SKILL)?;
    Ok(skill)
}

fn size_kb(path: &Path) -> f64 {
    fs::metadata(path).map_or(0.0, |m| m.len() as f64 / 1024.0)
}
