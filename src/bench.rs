use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::docs;
use crate::extract::is_doc_language;
use crate::index::{index_repo, read_source, resolve_root};
use crate::lookup::{self, Answer, BUNDLE_CAP, LOOKUP_CAP, SOURCE_CAP, View, fit, path_tier};
use crate::model::{Index, Symbol};
use crate::overview;

const BYTES_PER_TOKEN: u64 = 4;
const MIN_ANSWERED_PERCENT: u64 = 90;
const MAX_SEARCH_LINES: usize = 250;
const MAX_READ_LINES: usize = 2000;
// Agents mostly read documents in pieces, so that's the fair comparison, not reading the whole file.
const DOC_READ_LINES: usize = 100;
const DOC_QUERY_WORDS: usize = 3;

#[derive(Serialize, Deserialize, Clone)]
pub struct KindReport {
    pub task: String,
    pub tasks: u64,
    pub baseline_tokens: u64,
    pub drift_tokens: u64,
    pub saved_percent: f64,
    pub answered: u64,
    pub largest_drift_tokens: u64,
}

#[derive(Serialize, Deserialize)]
pub struct Report {
    pub drift_version: String,
    pub repo: String,
    pub commit: String,
    pub files: u64,
    pub bytes_per_token: u64,
    pub kinds: Vec<KindReport>,
    pub total: KindReport,
}

struct Measure {
    baseline: u64,
    drift: u64,
    answered: bool,
}

struct Corpus<'a> {
    sources: BTreeMap<&'a str, String>,
    doc_paths: Vec<&'a str>,
    view: View,
}

struct DocTask {
    path: String,
    start: u32,
    end: u32,
    words: Vec<String>,
}

// Words from a section's first sentence stand in for what an agent would ask about it. They come
// from the text and not the heading, so a search has to do more than match the title.
fn question_for(lines: &[&str]) -> Option<Vec<String>> {
    let mut in_code = false;
    for line in lines.iter().skip(1) {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code = !in_code;
            continue;
        }
        if in_code || line.starts_with("    ") {
            continue;
        }
        let words: Vec<String> = trimmed
            .split(|c: char| !c.is_alphabetic())
            .filter(|word| word.len() >= 4)
            .map(str::to_lowercase)
            .filter(|word| !docs::is_stop_word(word))
            .take(DOC_QUERY_WORDS)
            .collect();
        if words.len() == DOC_QUERY_WORDS {
            return Some(words);
        }
    }
    None
}

fn doc_tasks(index: &Index, corpus: &Corpus, count: usize) -> Vec<DocTask> {
    let mut tasks = Vec::new();
    for path in &corpus.doc_paths {
        let (Some(source), Some(file)) = (corpus.sources.get(path), index.files.get(*path)) else {
            continue;
        };
        if docs::doc_weight(path) < 1.0 {
            continue;
        }
        let lines: Vec<&str> = source.lines().collect();
        for section in docs::sections(file, &lines) {
            if section.end < section.start + 3 {
                continue;
            }
            let body = &lines[section.start as usize - 1..(section.end as usize).min(lines.len())];
            if let Some(words) = question_for(body) {
                tasks.push(DocTask {
                    path: (*path).to_string(),
                    start: section.start,
                    end: section.end,
                    words,
                });
            }
        }
    }
    (0..count.min(tasks.len()))
        .map(|i| i * tasks.len() / count.min(tasks.len()))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|at| tasks.swap_remove(at))
        .collect()
}

impl Corpus<'_> {
    // What a search would return, stopped where agent tools stop. Claude Code's Grep defaults to
    // 250 lines, so a baseline can't claim savings an agent would never have paid for.
    fn search_bytes(
        &self,
        accept_file: impl Fn(&str) -> bool,
        accept: impl Fn(&str) -> bool,
    ) -> u64 {
        let mut total = 0;
        let mut shown = 0;
        for (path, source) in &self.sources {
            if !accept_file(path) {
                continue;
            }
            for (number, line) in source.lines().enumerate() {
                if accept(line) {
                    if shown == MAX_SEARCH_LINES {
                        return total;
                    }
                    shown += 1;
                    total += (path.len() + digits(number + 1) + 2 + line.len() + 1) as u64;
                }
            }
        }
        total
    }

    fn grep_bytes(&self, needle: &str) -> u64 {
        self.search_bytes(|_| true, |line| line.contains(needle))
    }

    // Searching for what imports a module means looking for its name on import lines.
    fn import_search_bytes(&self, stem: &str) -> u64 {
        self.search_bytes(
            |_| true,
            |line| {
                line.contains(stem)
                    && [
                        "import", "require", "from ", "use ", "mod ", "#include", "preload(",
                        "using ",
                    ]
                    .iter()
                    .any(|word| line.contains(word))
            },
        )
    }

    fn test_search_bytes(&self, stem: &str) -> u64 {
        self.search_bytes(|path| path_tier(path) == 1, |line| line.contains(stem))
    }

    // Claude Code's Read stops at 2,000 lines by default.
    fn file_bytes(&self, path: &str) -> u64 {
        self.sources.get(path).map_or(0, |source| {
            source
                .lines()
                .take(MAX_READ_LINES)
                .map(|line| line.len() as u64 + 1)
                .sum()
        })
    }

    fn doc_search_bytes(&self, word: &str) -> u64 {
        let wanted = word.to_lowercase();
        self.search_bytes(
            |path| self.doc_paths.contains(&path),
            |line| line.to_lowercase().contains(&wanted),
        )
    }

    fn lines_bytes(&self, path: &str, start: u32, count: usize) -> u64 {
        self.sources.get(path).map_or(0, |source| {
            source
                .lines()
                .skip(start as usize - 1)
                .take(count)
                .map(|line| line.len() as u64 + 1)
                .sum()
        })
    }

    fn output(&self, answer: &Answer, cap: usize) -> String {
        fit(&answer.lines, &self.view, cap)
    }
}

fn digits(number: usize) -> usize {
    number.to_string().len()
}

// Evenly spaced picks over a sorted list, so the same repo always gets the same tasks.
fn spread<T: Clone>(items: &[T], count: usize) -> Vec<T> {
    if items.len() <= count {
        return items.to_vec();
    }
    (0..count)
        .map(|i| items[i * items.len() / count].clone())
        .collect()
}

// Overloads share a name in one file, so those need a line number to tell apart.
fn selector(index: &Index, path: &str, symbol: &Symbol) -> String {
    let same_name = index.files[path]
        .symbols
        .iter()
        .filter(|other| other.name == symbol.name)
        .count();
    if same_name > 1 {
        format!("{path}:{}", symbol.line)
    } else {
        format!("{path}:{}", symbol.name)
    }
}

fn summarize(task: &str, measures: &[Measure]) -> KindReport {
    let baseline: u64 = measures.iter().map(|m| m.baseline).sum();
    let drift: u64 = measures.iter().map(|m| m.drift).sum();
    KindReport {
        task: task.to_string(),
        tasks: measures.len() as u64,
        baseline_tokens: baseline / BYTES_PER_TOKEN,
        drift_tokens: drift / BYTES_PER_TOKEN,
        saved_percent: saved(baseline, drift),
        answered: measures.iter().filter(|m| m.answered).count() as u64,
        largest_drift_tokens: measures.iter().map(|m| m.drift).max().unwrap_or(0) / BYTES_PER_TOKEN,
    }
}

fn saved(baseline: u64, drift: u64) -> f64 {
    if baseline == 0 {
        return 0.0;
    }
    let percent = 100.0 * (baseline as f64 - drift as f64) / baseline as f64;
    (percent * 10.0).round() / 10.0
}

pub fn measure(root: &Path, per_kind: usize) -> Result<Report> {
    let index = index_repo(root, &[], &[])?;
    let mut sources = BTreeMap::new();
    for path in index.files.keys() {
        if let Some(source) = read_source(root, path) {
            sources.insert(path.as_str(), source);
        }
    }
    let doc_paths: Vec<&str> = index
        .files
        .iter()
        .filter(|(_, file)| is_doc_language(&file.lang))
        .map(|(path, _)| path.as_str())
        .collect();
    let corpus = Corpus {
        sources,
        doc_paths,
        view: View::default(),
    };

    let definitions: Vec<(&str, &Symbol)> = index
        .files
        .iter()
        .filter(|(path, _)| path_tier(path) == 0)
        .flat_map(|(path, file)| file.symbols.iter().map(move |s| (path.as_str(), s)))
        .filter(|(_, symbol)| {
            matches!(
                symbol.kind.as_str(),
                "function" | "class" | "struct" | "table"
            ) && symbol.exported
                && symbol.name.len() >= 4
                && symbol.end_line.unwrap_or(symbol.line) >= symbol.line + 2
        })
        .collect();
    let picked = spread(&definitions, per_kind);

    let mut kinds = Vec::new();

    let measures: Vec<Measure> = picked
        .iter()
        .map(|(path, symbol)| {
            let names = [symbol.name.clone()];
            let answer = lookup::query(&index, &names, &corpus.view);
            let out = corpus.output(&answer, LOOKUP_CAP);
            Measure {
                baseline: corpus.grep_bytes(&symbol.name),
                drift: out.len() as u64,
                answered: out.contains(&format!("{path}:{}", symbol.line)),
            }
        })
        .collect();
    kinds.push(summarize("find a definition", &measures));

    let measures: Vec<Measure> = picked
        .iter()
        .map(|(path, symbol)| {
            let answer = lookup::show(&index, &selector(&index, path, symbol), &corpus.view);
            let out = corpus.output(&answer, SOURCE_CAP);
            let first_line = corpus
                .sources
                .get(path)
                .and_then(|source| source.lines().nth(symbol.line as usize - 1).map(str::trim));
            Measure {
                baseline: corpus.file_bytes(path),
                drift: out.len() as u64,
                answered: first_line.is_some_and(|line| out.contains(line)),
            }
        })
        .collect();
    kinds.push(summarize("read one function", &measures));

    let measures: Vec<Measure> = picked
        .iter()
        .map(|(path, symbol)| {
            let names = [selector(&index, path, symbol)];
            let answer = lookup::sig(&index, &names, &corpus.view);
            let out = corpus.output(&answer, LOOKUP_CAP);
            Measure {
                baseline: corpus.file_bytes(path),
                drift: out.len() as u64,
                answered: symbol
                    .signature
                    .as_deref()
                    .is_some_and(|signature| out.contains(signature)),
            }
        })
        .collect();
    kinds.push(summarize("see a signature", &measures));

    let files: Vec<&str> = index
        .files
        .iter()
        .filter(|(path, file)| path_tier(path) == 0 && file.symbols.len() >= 3)
        .map(|(path, _)| path.as_str())
        .collect();
    let measures: Vec<Measure> = spread(&files, per_kind)
        .iter()
        .map(|path| {
            let answer = lookup::outline(&index, path, false);
            let out = corpus.output(&answer, LOOKUP_CAP);
            let first = &index.files[*path].symbols[0].name;
            Measure {
                baseline: corpus.file_bytes(path),
                drift: out.len() as u64,
                answered: out.contains(first.as_str()),
            }
        })
        .collect();
    kinds.push(summarize("understand a file", &measures));

    let graph = overview::import_graph(&index);
    let imported: Vec<(&String, &String)> = graph
        .iter()
        .filter(|(path, _)| path_tier(path) == 0)
        .filter_map(|(path, importers)| importers.iter().next().map(|first| (path, first)))
        .collect();
    let measures: Vec<Measure> = spread(&imported, per_kind)
        .iter()
        .map(|(path, importer)| {
            let answer = lookup::importers(&index, path, &corpus.view);
            let out = corpus.output(&answer, LOOKUP_CAP);
            let stem = path
                .rsplit('/')
                .next()
                .and_then(|file| file.split('.').next())
                .unwrap_or(path);
            Measure {
                baseline: corpus.import_search_bytes(stem),
                drift: out.len() as u64,
                answered: out.contains(importer.as_str()),
            }
        })
        .collect();
    if !measures.is_empty() {
        kinds.push(summarize("find importers", &measures));
    }

    let with_tests: Vec<(&str, &str)> = files
        .iter()
        .filter_map(|path| {
            lookup::tests_of(&index, path)
                .first()
                .map(|(test_path, _)| (*path, *test_path))
        })
        .collect();
    let measures: Vec<Measure> = spread(&with_tests, per_kind)
        .iter()
        .map(|(path, test_path)| {
            let answer = lookup::tests_for(&index, path);
            let out = corpus.output(&answer, LOOKUP_CAP);
            let stem = path
                .rsplit('/')
                .next()
                .and_then(|file| file.split('.').next())
                .unwrap_or(path);
            Measure {
                baseline: corpus.test_search_bytes(stem),
                drift: out.len() as u64,
                answered: out.contains(test_path),
            }
        })
        .collect();
    if !measures.is_empty() {
        kinds.push(summarize("find a file's tests", &measures));
    }

    let measures: Vec<Measure> = doc_tasks(&index, &corpus, per_kind)
        .iter()
        .map(|task| {
            let answer = docs::docs(&index, &task.words, &corpus.view);
            let search = corpus.output(&answer, LOOKUP_CAP);
            let pointer = format!("{}:{}-{}", task.path, task.start, task.end);
            let read = lookup::show(&index, &pointer, &corpus.view);
            let read = corpus.output(&read, SOURCE_CAP);
            // An agent searches for the most specific word it has, then reads from where it landed.
            let cheapest_search = task
                .words
                .iter()
                .map(|word| corpus.doc_search_bytes(word))
                .filter(|bytes| *bytes > 0)
                .min()
                .unwrap_or(0);
            Measure {
                baseline: cheapest_search
                    + corpus.lines_bytes(&task.path, task.start, DOC_READ_LINES),
                drift: (search.len() + read.len()) as u64,
                answered: search.contains(&pointer),
            }
        })
        .collect();
    if !measures.is_empty() {
        kinds.push(summarize("find in the docs", &measures));
    }

    let measures: Vec<Measure> = picked
        .iter()
        .map(|(_, symbol)| {
            let answer = lookup::refs(&index, &symbol.name, &corpus.view);
            let out = corpus.output(&answer, LOOKUP_CAP);
            let grep = corpus.grep_bytes(&symbol.name);
            Measure {
                baseline: grep,
                drift: out.len() as u64,
                // A definition with no uses is a correct answer, and tells the agent as much.
                answered: answer.found || out.contains("No references"),
            }
        })
        .collect();
    kinds.push(summarize("find uses", &measures));

    let answer = overview::map(&index, &corpus.view);
    let out = corpus.output(&answer, BUNDLE_CAP);
    let baseline: u64 = index.files.keys().map(|path| path.len() as u64 + 1).sum();
    kinds.push(summarize(
        "orient in the repo",
        &[Measure {
            baseline,
            drift: out.len() as u64,
            answered: answer.found,
        }],
    ));

    let total = {
        let baseline: u64 = kinds.iter().map(|k| k.baseline_tokens).sum();
        let drift: u64 = kinds.iter().map(|k| k.drift_tokens).sum();
        KindReport {
            task: "total".to_string(),
            tasks: kinds.iter().map(|k| k.tasks).sum(),
            baseline_tokens: baseline,
            drift_tokens: drift,
            saved_percent: saved(baseline, drift),
            answered: kinds.iter().map(|k| k.answered).sum(),
            largest_drift_tokens: kinds
                .iter()
                .map(|k| k.largest_drift_tokens)
                .max()
                .unwrap_or(0),
        }
    };

    Ok(Report {
        drift_version: env!("CARGO_PKG_VERSION").to_string(),
        repo: index.repo.clone(),
        commit: commit_of(root),
        files: index.files.len() as u64,
        bytes_per_token: BYTES_PER_TOKEN,
        kinds,
        total,
    })
}

fn commit_of(root: &Path) -> String {
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
    };
    let Some(commit) = git(&["rev-parse", "--short", "HEAD"]) else {
        return "unknown".to_string();
    };
    let dirty = git(&["status", "--porcelain"]).is_some_and(|status| !status.is_empty());
    if dirty {
        format!("{commit}+changes")
    } else {
        commit
    }
}

fn print_table(report: &Report, before: Option<&Report>) {
    println!(
        "drift bench  {}@{}  drift {}  {} files  ~{} bytes per token",
        report.repo, report.commit, report.drift_version, report.files, report.bytes_per_token
    );
    if before.is_some() {
        println!(
            "{:<22}{:>6}{:>11}{:>11}{:>9}{:>7}{:>10}",
            "task", "runs", "without", "drift", "saved", "right", "vs before"
        );
    } else {
        println!(
            "{:<22}{:>6}{:>11}{:>11}{:>9}{:>7}",
            "task", "runs", "without", "drift", "saved", "right"
        );
    }
    for kind in report.kinds.iter().chain([&report.total]) {
        let change = before.map(|before| {
            before
                .kinds
                .iter()
                .chain([&before.total])
                .find(|old| old.task == kind.task)
                .map_or("new".to_string(), |old| {
                    if old.drift_tokens == 0 {
                        return "-".to_string();
                    }
                    let percent = 100.0 * (kind.drift_tokens as f64 - old.drift_tokens as f64)
                        / old.drift_tokens as f64;
                    format!("{percent:+.1}%")
                })
        });
        print!(
            "{:<22}{:>6}{:>11}{:>11}{:>8.1}%{:>7}",
            kind.task,
            kind.tasks,
            kind.baseline_tokens,
            kind.drift_tokens,
            kind.saved_percent,
            format!("{}/{}", kind.answered, kind.tasks)
        );
        match change {
            Some(change) => println!("{change:>10}"),
            None => println!(),
        }
    }
    println!(
        "Tokens of tool output without Drift (search capped at {MAX_SEARCH_LINES} lines, or reading the file) and with it. \
         'right' counts answers containing the result the index expects."
    );
}

pub fn run(
    path: &Path,
    per_kind: usize,
    json: bool,
    compare: Option<&Path>,
    check: Option<f64>,
) -> Result<()> {
    let root = resolve_root(path)?;
    let report = measure(&root, per_kind)?;

    let before: Option<Report> = match compare {
        Some(path) => {
            let text =
                fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
            Some(
                serde_json::from_str(&text)
                    .with_context(|| format!("{} is not a bench report", path.display()))?,
            )
        }
        None => None,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_table(&report, before.as_ref());
    }

    if let Some(minimum) = check {
        let answered_percent = 100 * report.total.answered / report.total.tasks.max(1);
        if report.total.saved_percent < minimum || answered_percent < MIN_ANSWERED_PERCENT {
            eprintln!(
                "bench check failed: saved {:.1}% (needs {minimum:.1}%), right {answered_percent}% (needs {MIN_ANSWERED_PERCENT}%)",
                report.total.saved_percent
            );
            std::process::exit(1);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{saved, spread};

    #[test]
    fn picks_are_even_and_repeatable() {
        let items: Vec<u32> = (0..100).collect();
        assert_eq!(spread(&items, 4), vec![0, 25, 50, 75]);
        assert_eq!(spread(&items, 4), spread(&items, 4));
        assert_eq!(spread(&items[..2], 4), vec![0, 1]);
    }

    #[test]
    fn savings_are_percentages_of_the_baseline() {
        assert_eq!(saved(1000, 250), 75.0);
        assert_eq!(saved(0, 10), 0.0);
        assert_eq!(saved(100, 200), -100.0);
    }
}
