#[cfg(target_env = "musl")]
mod allocator;
mod audit;
mod bench;
mod extract;
mod imports;
mod index;
mod install;
mod lookup;
mod model;
mod overview;
mod update;

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Result, ensure};
use clap::{Args, Parser, Subcommand};

use index::{INDEX_FILE, index_repo, load_index, resolve_root, write_index};
use lookup::{BUNDLE_CAP, LOOKUP_CAP, SOURCE_CAP, View, deliver};
use model::Index;

#[derive(Parser)]
#[command(
    name = "drift",
    version,
    about = "Find code with fewer tokens: an index and lookups for AI agents.",
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Args)]
struct LookupArgs {
    /// Use this index file as is, without refreshing it
    #[arg(long)]
    index: Option<PathBuf>,
    /// Show at most this many lines instead of the default size cap
    #[arg(long)]
    limit: Option<usize>,
    /// Show everything, ignoring the default size cap
    #[arg(long)]
    all: bool,
    /// Only look in this folder
    #[arg(long = "in", value_name = "PATH")]
    within: Option<String>,
}

impl LookupArgs {
    fn view(&self) -> View {
        View {
            limit: self.limit,
            all: self.all,
            within: self.within.clone(),
        }
    }

    fn load(&self) -> Result<Index> {
        load_index(self.index.as_deref())
    }
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
    /// Look up where symbols are defined
    Query {
        /// Symbol names; each falls back to a case-insensitive substring match
        #[arg(required = true)]
        symbols: Vec<String>,
        #[command(flatten)]
        args: LookupArgs,
    },
    /// Search for definitions by words in any order, such as: rate limit
    Find {
        /// Words to look for in names, doc comments and paths
        #[arg(required = true)]
        words: Vec<String>,
        #[command(flatten)]
        args: LookupArgs,
    },
    /// Print a definition's source
    Show {
        /// A name, Parent.name or path:name
        symbol: String,
        #[command(flatten)]
        args: LookupArgs,
    },
    /// Print only a definition's signature and doc comment
    Sig {
        /// Names, as Parent.name or path:name if needed
        #[arg(required = true)]
        symbols: Vec<String>,
        #[command(flatten)]
        args: LookupArgs,
    },
    /// List a file's definitions and imports without reading it
    Outline {
        /// File path, or a trailing part of one such as lib/auth.ts
        file: String,
        /// Show each definition's signature instead of its name
        #[arg(long)]
        sig: bool,
        #[command(flatten)]
        args: LookupArgs,
    },
    /// Find uses of a symbol, by file and enclosing definition
    Refs {
        /// Symbol name
        symbol: String,
        #[command(flatten)]
        args: LookupArgs,
    },
    /// List test files, or find tests by path, suite or case name
    Tests {
        /// Case-insensitive text to look for; omit to summarize every test file
        pattern: Option<String>,
        /// Find the tests for this source file instead
        #[arg(long = "for", value_name = "FILE", conflicts_with = "pattern")]
        for_file: Option<String>,
        #[command(flatten)]
        args: LookupArgs,
    },
    /// Find the files that import a module
    Importers {
        /// File path such as src/lib/auth.ts, or text to find in import sources
        module: String,
        #[command(flatten)]
        args: LookupArgs,
    },
    /// Everything needed before changing a symbol or file
    Context {
        /// A symbol or a file
        target: String,
        #[command(flatten)]
        args: LookupArgs,
    },
    /// Summarize a repo: folders, entry points and most imported files
    Map {
        #[command(flatten)]
        args: LookupArgs,
    },
    /// List the definitions changed since a git ref (default: HEAD)
    Changes {
        /// Git ref to compare with
        reference: Option<String>,
        #[command(flatten)]
        args: LookupArgs,
    },
    /// Measure token savings on a repo with a standard set of tasks
    Bench {
        /// Path to repo root
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Tasks to run of each kind
        #[arg(long, default_value_t = 10)]
        tasks: usize,
        /// Print the report as JSON, to save for --compare
        #[arg(long)]
        json: bool,
        /// Compare with a report saved earlier using --json
        #[arg(long, value_name = "FILE")]
        compare: Option<PathBuf>,
        /// Fail unless total savings reach this percent and 90% of answers are right
        #[arg(long, value_name = "PERCENT")]
        check: Option<f64>,
    },
    /// Report where agent tokens went, from local Claude Code transcripts
    Audit {
        /// How many days back to look
        #[arg(long, default_value_t = 30)]
        days: u64,
        /// Only count projects whose transcript folder contains this text
        #[arg(long, value_name = "TEXT")]
        project: Option<String>,
        /// Read transcripts from this folder instead of ~/.claude/projects
        #[arg(long, value_name = "DIR")]
        dir: Option<PathBuf>,
        /// Print the totals as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show index statistics
    Stats {
        #[command(flatten)]
        args: LookupArgs,
    },
    /// Index every git repo directly under a parent directory
    Batch {
        /// Parent directory containing repos
        parent: PathBuf,
        /// Compact output
        #[arg(long)]
        compact: bool,
        /// Also install the skills in each repo
        #[arg(long)]
        install: bool,
    },
    /// Set up a repo so coding agents use Drift
    Install {
        /// Path to repo root
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Also add a Claude Code hook that points agents to Drift
        #[arg(long)]
        hook: bool,
        /// Also add instructions to AGENTS.md, and to CLAUDE.md and GEMINI.md if the repo has them
        #[arg(long)]
        instructions: bool,
        /// Put the instructions in this file instead
        #[arg(long, value_name = "FILE", requires = "instructions")]
        file: Option<PathBuf>,
        /// Also set up these tools before the repo uses them: kiro, junie, continue, aider or all
        #[arg(long, value_name = "TOOL", value_delimiter = ',')]
        agent: Vec<String>,
    },
    /// Check for a newer release and show how to install it
    UpdateCheck,
    /// Used by the Claude Code hook; reads an event on stdin
    #[command(hide = true)]
    Hook,
    /// Used by the background update check
    #[command(hide = true, name = "__update-refresh")]
    UpdateRefresh,
}

// `drift refs x | head` closes the pipe early. That's normal, not a crash worth reporting.
fn exit_quietly_on_closed_pipe() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if info.to_string().contains("failed printing to stdout") {
            std::process::exit(0);
        }
        default_hook(info);
    }));
}

fn main() -> Result<()> {
    exit_quietly_on_closed_pipe();
    let command = Cli::parse().command;
    // The hook runs on every search and speaks JSON, and the other two already deal with updates.
    let quiet = matches!(
        command,
        Command::Hook | Command::UpdateCheck | Command::UpdateRefresh
    );
    let result = run(command);
    if !quiet {
        update::after_command();
    }
    result
}

fn run(command: Command) -> Result<()> {
    match command {
        Command::Index {
            path,
            output,
            ignore_dirs,
            ignore_files,
            compact,
        } => cmd_index(&path, output, &ignore_dirs, &ignore_files, compact),
        Command::Query { symbols, args } => {
            let view = args.view();
            deliver(
                lookup::query(&args.load()?, &symbols, &view),
                &view,
                LOOKUP_CAP,
            );
            Ok(())
        }
        Command::Find { words, args } => {
            let view = args.view();
            deliver(
                lookup::find(&args.load()?, &words, &view),
                &view,
                LOOKUP_CAP,
            );
            Ok(())
        }
        Command::Show { symbol, args } => {
            let view = args.view();
            deliver(
                lookup::show(&args.load()?, &symbol, &view),
                &view,
                SOURCE_CAP,
            );
            Ok(())
        }
        Command::Sig { symbols, args } => {
            let view = args.view();
            deliver(
                lookup::sig(&args.load()?, &symbols, &view),
                &view,
                LOOKUP_CAP,
            );
            Ok(())
        }
        Command::Outline { file, sig, args } => {
            let view = args.view();
            deliver(
                lookup::outline(&args.load()?, &file, sig),
                &view,
                LOOKUP_CAP,
            );
            Ok(())
        }
        Command::Refs { symbol, args } => {
            let view = args.view();
            deliver(
                lookup::refs(&args.load()?, &symbol, &view),
                &view,
                LOOKUP_CAP,
            );
            Ok(())
        }
        Command::Tests {
            pattern,
            for_file,
            args,
        } => {
            let view = args.view();
            let index = args.load()?;
            let answer = match for_file {
                Some(file) => lookup::tests_for(&index, &file),
                None => lookup::tests(&index, pattern.as_deref(), &view),
            };
            deliver(answer, &view, LOOKUP_CAP);
            Ok(())
        }
        Command::Importers { module, args } => {
            let view = args.view();
            deliver(
                lookup::importers(&args.load()?, &module, &view),
                &view,
                LOOKUP_CAP,
            );
            Ok(())
        }
        Command::Context { target, args } => {
            let view = args.view();
            deliver(
                lookup::context(&args.load()?, &target, &view),
                &view,
                BUNDLE_CAP,
            );
            Ok(())
        }
        Command::Map { args } => {
            let view = args.view();
            deliver(overview::map(&args.load()?, &view), &view, BUNDLE_CAP);
            Ok(())
        }
        Command::Changes { reference, args } => {
            let view = args.view();
            let answer = overview::changes(&args.load()?, reference.as_deref(), &view)?;
            deliver(answer, &view, LOOKUP_CAP);
            Ok(())
        }
        Command::Bench {
            path,
            tasks,
            json,
            compare,
            check,
        } => bench::run(&path, tasks, json, compare.as_deref(), check),
        Command::Audit {
            days,
            project,
            dir,
            json,
        } => audit::run(days, project.as_deref(), dir.as_deref(), json),
        Command::Stats { args } => cmd_stats(&args.load()?),
        Command::Batch {
            parent,
            compact,
            install,
        } => cmd_batch(&parent, compact, install),
        Command::Install {
            path,
            hook,
            instructions,
            file,
            agent,
        } => cmd_install(&path, hook, instructions, file.as_deref(), &agent),
        Command::UpdateCheck => update::check_now(),
        Command::Hook => {
            install::hook();
            Ok(())
        }
        Command::UpdateRefresh => {
            update::refresh();
            Ok(())
        }
    }
}

fn cmd_install(
    path: &Path,
    hook: bool,
    instructions: bool,
    file: Option<&Path>,
    agents: &[String],
) -> Result<()> {
    let root = resolve_root(path)?;
    for skill in install::install_skills(&root)? {
        println!("Skill: {}", skill.display());
    }
    if install::ignore_index(&root)? {
        println!("Added {INDEX_FILE} to .gitignore");
    }
    for line in install::install_tools(&root, agents)? {
        println!("{line}");
    }
    if instructions {
        for target in install::instructions_targets(&root, file) {
            install::install_instructions(&target)?;
            println!("Instructions: {}", target.display());
        }
    }
    if hook {
        println!("Hook: {}", install::install_hook(&root)?.display());
    }
    Ok(())
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

fn cmd_stats(index: &Index) -> Result<()> {
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
        install::install_skills(repo)?;
    }
    Ok((index, output, elapsed))
}

fn size_kb(path: &Path) -> f64 {
    fs::metadata(path).map_or(0.0, |m| m.len() as f64 / 1024.0)
}
