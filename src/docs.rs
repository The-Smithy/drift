//! Finds the part of a document that answers a question. Document text isn't in the index, which
//! every lookup has to load, so it's read here instead.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use rayon::prelude::*;

use crate::extract::{self, prose};
use crate::index::read_source;
use crate::lookup::{Answer, View, counted, name_words};
use crate::model::{FileEntry, Index, Symbol};

const SEARCH_HITS: usize = 8;
const HITS_PER_FILE: usize = 3;
const LONG_SECTION: usize = 120;
const MAX_LISTED: usize = 30;
const MAX_SNIPPET: usize = 110;
const MAX_LABEL: usize = 70;
const MAX_SUMMARY: usize = 100;
// Hits far below the best one are noise that costs tokens to rule out.
const RELATIVE_CUT: f64 = 0.15;
const GIT_DEADLINE: Duration = Duration::from_secs(2);
const MAX_PATHSPEC: usize = 200;

// BM25's usual settings, and how much a word counts in places that say what a section is about.
const K1: f64 = 1.2;
const B: f64 = 0.75;
const TITLE_WEIGHT: f64 = 2.0;
const TRAIL_WEIGHT: f64 = 0.75;
const FILE_WEIGHT: f64 = 1.5;
const FOLDER_WEIGHT: f64 = 0.25;
const PHRASE_WEIGHT: f64 = 1.0;

const STOP_WORDS: &[&str] = &[
    "a", "about", "all", "an", "and", "any", "are", "as", "at", "be", "but", "by", "can", "do",
    "does", "for", "from", "how", "i", "if", "in", "into", "is", "it", "its", "me", "my", "need",
    "needs", "not", "of", "on", "or", "our", "should", "so", "that", "the", "their", "them",
    "then", "there", "these", "this", "to", "use", "used", "using", "via", "was", "we", "what",
    "when", "where", "which", "who", "why", "will", "with", "you", "your",
];

/// One readable stretch of a document.
pub struct Section {
    pub title: String,
    pub trail: Vec<String>,
    pub start: u32,
    pub end: u32,
}

pub struct Hit {
    pub path: String,
    pub title: String,
    pub trail: Vec<String>,
    pub start: u32,
    pub end: u32,
    pub score: f64,
    pub missing: Vec<String>,
}

pub struct Search {
    pub hits: Vec<Hit>,
    terms: Vec<String>,
    sources: HashMap<String, String>,
}

// Close enough for "refunds" to find "refund" and "configuring" to find "configuration". It only
// has to treat both sides of a comparison the same, so odd stems don't matter.
fn stem(word: &str) -> String {
    if !word.is_ascii() || word.len() <= 3 {
        return word.to_string();
    }
    let mut stemmed = word;
    let owned;
    if let Some(root) = word.strip_suffix("ies").filter(|root| root.len() >= 2) {
        owned = format!("{root}y");
        stemmed = &owned;
    } else if let Some(root) = word.strip_suffix("sses") {
        owned = format!("{root}ss");
        stemmed = &owned;
    } else if word.ends_with('s')
        && !word.ends_with("ss")
        && !word.ends_with("us")
        && !word.ends_with("is")
    {
        stemmed = &word[..word.len() - 1];
    }
    let mut root = stemmed;
    for suffix in ["ing", "ed", "ment"] {
        if let Some(shorter) = root.strip_suffix(suffix).filter(|s| s.len() >= 3) {
            root = shorter;
            break;
        }
    }
    if root.len() == stemmed.len()
        && let Some(shorter) = root
            .strip_suffix("ion")
            .filter(|s| s.len() >= 4 && (s.ends_with('t') || s.ends_with('s')))
    {
        root = shorter;
    }
    if root.len() > 6 {
        root = root.strip_suffix("at").unwrap_or(root);
    }
    // running -> run, stopped -> stop.
    let bytes = root.as_bytes();
    if root.len() != stemmed.len() && bytes.len() >= 4 {
        let last = bytes[bytes.len() - 1];
        if last == bytes[bytes.len() - 2] && !matches!(last, b'l' | b's' | b'z') {
            root = &root[..root.len() - 1];
        }
    }
    if root.len() > 4 {
        root = root.strip_suffix('e').unwrap_or(root);
    }
    root.to_string()
}

fn has_case_change(word: &str) -> bool {
    let mut previous_lower = false;
    for c in word.chars() {
        if c.is_uppercase() && previous_lower {
            return true;
        }
        previous_lower = c.is_lowercase();
    }
    false
}

fn each_token(text: &str, mut visit: impl FnMut(String)) {
    for word in text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        visit(stem(&word.to_lowercase()));
        if has_case_change(word) {
            for part in name_words(word) {
                visit(stem(&part));
            }
        }
    }
}

// Each word's stem, with the word as typed for telling the agent which ones a hit lacks. A word
// spelled like an identifier is searched whole, and plain words also match inside identifiers.
fn query_terms(words: &[String]) -> Vec<(String, String)> {
    let raw: Vec<String> = words
        .iter()
        .flat_map(|word| word.split(|c: char| !c.is_alphanumeric()))
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect();
    let meaningful: Vec<&String> = raw
        .iter()
        .filter(|word| !STOP_WORDS.contains(&word.as_str()))
        .collect();
    let kept = if meaningful.is_empty() {
        raw.iter().collect()
    } else {
        meaningful
    };
    let mut seen = HashSet::new();
    kept.into_iter()
        .map(|word| (stem(word), word.clone()))
        .filter(|(term, _)| seen.insert(term.clone()))
        .collect()
}

pub fn is_stop_word(word: &str) -> bool {
    STOP_WORDS.contains(&word)
}

pub fn doc_weight(path: &str) -> f64 {
    let lowered = path.to_lowercase();
    let mut segments = lowered.split('/');
    let file = segments.next_back().unwrap_or("");
    let stem = file.split('.').next().unwrap_or("");
    if matches!(
        stem,
        "changelog"
            | "changes"
            | "history"
            | "releases"
            | "release-notes"
            | "release_notes"
            | "news"
    ) {
        return 0.4;
    }
    let low = [
        "fixtures",
        "fixture",
        "testdata",
        "__snapshots__",
        "golden",
        "template",
        "templates",
        "tests",
        "test",
    ];
    if segments.any(|directory| low.contains(&directory)) {
        return 0.5;
    }
    1.0
}

fn is_excluded(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    let stem = file.split('.').next().unwrap_or("");
    matches!(
        stem,
        "license" | "licence" | "copying" | "notice" | "third-party-notices"
    )
}

fn front_matter<'a>(lines: &[&'a str], field: &str) -> Option<&'a str> {
    let marker = lines.first()?.trim_end();
    if marker != "---" && marker != "+++" {
        return None;
    }
    lines
        .iter()
        .skip(1)
        .take_while(|line| line.trim_end() != marker)
        .find_map(|line| {
            let rest = line.strip_prefix(field)?.trim_start();
            let value = rest.strip_prefix([':', '='])?.trim();
            let value = value.trim_matches(['"', '\'']);
            (!value.is_empty()).then_some(value)
        })
}

fn has_text(lines: &[&str], from: usize, to: usize) -> bool {
    lines[from.min(lines.len())..to.min(lines.len())]
        .iter()
        .any(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty() && !trimmed.chars().all(|c| c.is_ascii_punctuation())
        })
}

fn trail_of<'a>(symbol: &'a Symbol, parents: &HashMap<&'a str, Option<&'a str>>) -> Vec<String> {
    let mut trail = Vec::new();
    let mut parent = symbol.parent.as_deref();
    while let Some(name) = parent {
        if trail.len() == 6 {
            break;
        }
        trail.insert(0, name.to_string());
        parent = parents.get(name).copied().flatten();
    }
    trail
}

/// A document's readable stretches, in order. Unlike a heading's own range, which runs through its
/// subsections, each one stops at the next heading of any level, and long ones are split.
pub fn sections(file: &FileEntry, lines: &[&str]) -> Vec<Section> {
    let mut found = Vec::new();
    let Some(first) = file.symbols.first() else {
        return found;
    };
    let mut push = |title: &str, trail: &[String], from: usize, to: usize| {
        let pieces = if to.saturating_sub(from) > LONG_SECTION {
            prose::chunks(lines, from, to)
        } else {
            vec![(from, to.saturating_sub(1))]
        };
        for (start, end) in pieces {
            found.push(Section {
                title: title.to_string(),
                trail: trail.to_vec(),
                start: start as u32 + 1,
                end: end.max(start) as u32 + 1,
            });
        }
    };

    let first_line = first.line as usize - 1;
    if has_text(lines, 0, first_line) {
        let title = front_matter(lines, "title").unwrap_or("(start of file)");
        push(title, &[], 0, first_line);
    }

    let mut parents: HashMap<&str, Option<&str>> = HashMap::new();
    for (at, symbol) in file.symbols.iter().enumerate() {
        let next = file.symbols.get(at + 1);
        let from = symbol.line as usize - 1;
        let mut to = next.map_or(lines.len(), |next| next.line as usize - 1);
        while to > from + 1 && lines.get(to - 1).is_none_or(|line| line.trim().is_empty()) {
            to -= 1;
        }
        let trail = trail_of(symbol, &parents);
        parents.insert(&symbol.name, symbol.parent.as_deref());

        // A heading with nothing under it but subheadings has nothing to read yet, and its words
        // still count through its subsections' trails.
        let starts_child = next.is_some_and(|next| next.parent.as_deref() == Some(&symbol.name));
        if symbol.kind == "heading" && starts_child && !has_text(lines, from + 1, to) {
            continue;
        }
        push(&symbol.name, &trail, from, to);
    }
    found
}

#[derive(Default)]
struct Counts {
    frequency: Vec<u32>,
    in_title: Vec<bool>,
    in_trail: Vec<bool>,
    in_file: Vec<bool>,
    in_folder: Vec<bool>,
    phrases: u32,
    length: u32,
}

fn count(terms: &[String], path: &str, section: &Section, lines: &[&str]) -> Counts {
    let position = |token: &str| terms.iter().position(|term| term == token);
    let mark = |text: &str| {
        let mut marks = vec![false; terms.len()];
        each_token(text, |token| {
            if let Some(at) = position(&token) {
                marks[at] = true;
            }
        });
        marks
    };
    let (folder, file_name) = path.rsplit_once('/').unwrap_or(("", path));
    let mut counts = Counts {
        frequency: vec![0; terms.len()],
        in_title: mark(&section.title),
        in_trail: mark(&section.trail.join(" ")),
        in_file: mark(&name_words(file_name).join(" ")),
        in_folder: mark(&name_words(folder).join(" ")),
        ..Counts::default()
    };
    let mut previous = None;
    for line in &lines[section.start as usize - 1..(section.end as usize).min(lines.len())] {
        each_token(line, |token| {
            counts.length += 1;
            let at = position(&token);
            if let Some(at) = at {
                counts.frequency[at] += 1;
                if previous.is_some_and(|before: usize| before != at) {
                    counts.phrases += 1;
                }
            }
            previous = at;
        });
    }
    counts
}

// A document's path and text, with its sections and what was counted in each.
type Scanned = (String, String, Vec<(Section, Counts)>);

pub fn search(index: &Index, words: &[String], view: &View) -> Search {
    let (terms, typed): (Vec<String>, Vec<String>) = query_terms(words).into_iter().unzip();
    let root = Path::new(&index.root);
    let scanned: Vec<Scanned> = doc_files(index, view)
        .into_par_iter()
        .filter_map(|(path, file)| {
            let source = read_source(root, path)?;
            let lines: Vec<&str> = source.lines().collect();
            let counted: Vec<(Section, Counts)> = sections(file, &lines)
                .into_iter()
                .map(|section| {
                    let counts = count(&terms, path, &section, &lines);
                    (section, counts)
                })
                .collect();
            drop(lines);
            Some((path.to_string(), source, counted))
        })
        .collect();

    let total: usize = scanned.iter().map(|(_, _, found)| found.len()).sum();
    let average = scanned
        .iter()
        .flat_map(|(_, _, found)| found)
        .map(|(_, counts)| f64::from(counts.length))
        .sum::<f64>()
        / total.max(1) as f64;
    let rarity: Vec<f64> = (0..terms.len())
        .map(|term| {
            let containing = scanned
                .iter()
                .flat_map(|(_, _, found)| found)
                .filter(|(_, counts)| {
                    counts.frequency[term] > 0 || counts.in_title[term] || counts.in_file[term]
                })
                .count() as f64;
            (1.0 + (total as f64 - containing + 0.5) / (containing + 0.5)).ln()
        })
        .collect();

    let mut sources = HashMap::new();
    let mut hits = Vec::new();
    for (path, source, found) in scanned {
        for (section, counts) in found {
            let mut score = 0.0;
            let mut missing = Vec::new();
            let mut matched = 0;
            for (term, weight) in rarity.iter().enumerate() {
                let frequency = f64::from(counts.frequency[term]);
                let length = f64::from(counts.length) / average.max(1.0);
                let body =
                    frequency * (K1 + 1.0) / (frequency + K1 * (1.0 - B + B * length)).max(0.01);
                let bonus = f64::from(u8::from(counts.in_title[term])) * TITLE_WEIGHT
                    + f64::from(u8::from(counts.in_trail[term])) * TRAIL_WEIGHT
                    + f64::from(u8::from(counts.in_file[term])) * FILE_WEIGHT
                    + f64::from(u8::from(counts.in_folder[term])) * FOLDER_WEIGHT;
                if counts.frequency[term] > 0
                    || counts.in_title[term]
                    || counts.in_trail[term]
                    || counts.in_file[term]
                    || counts.in_folder[term]
                {
                    matched += 1;
                } else {
                    missing.push(typed[term].clone());
                }
                score += weight * (body + bonus);
            }
            if matched == 0 {
                continue;
            }
            let average_rarity = rarity.iter().sum::<f64>() / rarity.len().max(1) as f64;
            score += f64::from(counts.phrases.min(3)) * PHRASE_WEIGHT * average_rarity;
            let coverage = f64::from(matched) / terms.len() as f64;
            score *= coverage.powf(1.5) * doc_weight(&path);
            hits.push(Hit {
                path: path.clone(),
                title: section.title,
                trail: section.trail,
                start: section.start,
                end: section.end,
                score,
                missing,
            });
        }
        sources.insert(path, source);
    }

    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.start.cmp(&b.start))
    });
    if let Some(best) = hits.first().map(|hit| hit.score) {
        hits.retain(|hit| hit.score >= best * RELATIVE_CUT);
    }
    Search {
        hits,
        terms,
        sources,
    }
}

fn doc_files<'a>(index: &'a Index, view: &View) -> Vec<(&'a str, &'a FileEntry)> {
    index
        .files
        .iter()
        .filter(|(path, file)| {
            extract::is_doc_language(&file.lang) && view.includes(path) && !is_excluded(path)
        })
        .map(|(path, file)| (path.as_str(), file))
        .collect()
}

fn clip(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let kept: String = text.chars().take(limit.saturating_sub(3)).collect();
    format!("{}...", kept.trim_end())
}

fn tidy(line: &str) -> String {
    let cleaned = extract::clean_title(line);
    cleaned
        .trim_start_matches(['#', '>', '-', '+', '|', '=', ' '])
        .trim_end_matches(['|', ' '])
        .to_string()
}

fn is_markup(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty()
        || trimmed.starts_with("```")
        || trimmed.starts_with("~~~")
        || trimmed.starts_with(".. ")
        || trimmed.chars().all(|c| c.is_ascii_punctuation())
}

// The line that matches the most words, cut so the first match stays in view.
fn snippet(lines: &[&str], hit: &Hit, terms: &[String]) -> String {
    let body = &lines[hit.start as usize - 1..(hit.end as usize).min(lines.len())];
    let mut best = None;
    let mut best_count = 0;
    let mut in_front_matter = false;
    for (at, line) in body.iter().enumerate() {
        if hit.start == 1 && at == 0 && matches!(line.trim_end(), "---" | "+++") {
            in_front_matter = true;
            continue;
        }
        if in_front_matter {
            in_front_matter = !matches!(line.trim_end(), "---" | "+++");
            continue;
        }
        if is_markup(line) || (at == 0 && hit.title == tidy(line)) {
            continue;
        }
        let mut matched = HashSet::new();
        each_token(line, |token| {
            if terms.contains(&token) {
                matched.insert(token);
            }
        });
        if best.is_none() || matched.len() > best_count {
            best = Some(*line);
            best_count = matched.len();
        }
    }
    let Some(line) = best else {
        return String::new();
    };
    let text = tidy(line);
    if text.chars().count() <= MAX_SNIPPET {
        return text;
    }
    let lowered = text.to_lowercase();
    let first_match = terms
        .iter()
        .filter_map(|term| lowered.find(term.as_str()))
        .min()
        .map_or(0, |byte| lowered[..byte].chars().count());
    let skip = first_match.saturating_sub(30);
    let window: String = text.chars().skip(skip).collect();
    let clipped = clip(&window, MAX_SNIPPET);
    if skip > 0 {
        format!("...{clipped}")
    } else {
        clipped
    }
}

fn file_date(root: &Path, path: &str) -> Option<String> {
    let modified = std::fs::metadata(root.join(path)).ok()?.modified().ok()?;
    let stamp = humantime::format_rfc3339_seconds(modified).to_string();
    Some(stamp.get(..10)?.to_string())
}

// When each file last changed, so an agent can weigh an old document. A file's own timestamp only
// says when it was checked out, so git comes first, and it stops being asked once it's taken too
// long. Those files get no date rather than a misleading one.
fn last_changed(root: &Path, paths: &[&str]) -> HashMap<String, String> {
    let mut dates = HashMap::new();
    let mut wanted: HashSet<&str> = paths.iter().copied().collect();
    let mut command = Command::new("git");
    command
        .args([
            "-c",
            "core.quotepath=off",
            "log",
            "--format=@%cs",
            "--name-only",
            "--relative",
        ])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if paths.len() <= MAX_PATHSPEC {
        command.arg("--").args(paths);
    }

    let mut finished = true;
    if let Ok(mut child) = command.spawn() {
        if let Some(stdout) = child.stdout.take() {
            let (sender, receiver) = mpsc::channel();
            thread::spawn(move || {
                use std::io::BufRead;
                for line in std::io::BufReader::new(stdout)
                    .lines()
                    .map_while(Result::ok)
                {
                    if sender.send(line).is_err() {
                        break;
                    }
                }
            });
            let deadline = Instant::now() + GIT_DEADLINE;
            let mut date = String::new();
            while !wanted.is_empty() {
                let left = deadline.saturating_duration_since(Instant::now());
                match receiver.recv_timeout(left) {
                    Ok(line) => {
                        if let Some(stamp) = line.strip_prefix('@') {
                            date = stamp.to_string();
                        } else if wanted.remove(line.as_str()) {
                            dates.insert(line, date.clone());
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        finished = false;
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    // Git had its say, so what's left isn't committed yet, or this isn't a git repo.
    if finished {
        for path in wanted {
            if let Some(date) = file_date(root, path) {
                dates.insert(path.to_string(), date);
            }
        }
    }
    dates
}

fn hit_label(hit: &Hit) -> String {
    let mut parts: Vec<&str> = hit.trail.iter().rev().take(1).map(String::as_str).collect();
    parts.push(&hit.title);
    clip(&parts.join(" > "), MAX_LABEL)
}

fn format_search(index: &Index, words: &[String], view: &View) -> Answer {
    let found = search(index, words, view);
    if found.terms.is_empty() {
        return Answer {
            lines: vec!["Nothing to search for".to_string()],
            found: false,
        };
    }
    if found.hits.is_empty() {
        return Answer {
            lines: vec![format!("No documents mention '{}'", words.join(" "))],
            found: false,
        };
    }

    let capped = !view.all && view.limit.is_none();
    let mut per_file: HashMap<&str, usize> = HashMap::new();
    let mut shown: Vec<&Hit> = Vec::new();
    let mut left_out = 0;
    for hit in &found.hits {
        let in_file = per_file.entry(&hit.path).or_default();
        if capped && (shown.len() == SEARCH_HITS || *in_file == HITS_PER_FILE) {
            left_out += 1;
            continue;
        }
        *in_file += 1;
        shown.push(hit);
    }

    let mut paths: Vec<&str> = shown.iter().map(|hit| hit.path.as_str()).collect();
    paths.sort_unstable();
    paths.dedup();
    let dates = last_changed(Path::new(&index.root), &paths);

    let mut lines: Vec<String> = shown
        .iter()
        .map(|hit| {
            let source_lines: Vec<&str> = found.sources[&hit.path].lines().collect();
            let mut line = format!("{}:{}-{}  {}", hit.path, hit.start, hit.end, hit_label(hit));
            if let Some(date) = dates.get(&hit.path) {
                line.push_str(&format!("  ({date})"));
            }
            if !hit.missing.is_empty() {
                line.push_str(&format!("  [no {}]", hit.missing.join(", ")));
            }
            let text = snippet(&source_lines, hit, &found.terms);
            if !text.is_empty() {
                line.push_str(&format!("  # {text}"));
            }
            line
        })
        .collect();
    if view.limit.is_none() {
        if left_out > 0 {
            lines.push(format!(
                "... {} more. Narrow with --in <path>, or use --all.",
                counted(left_out, "section")
            ));
        }
        lines.push("Read one with: drift show <path:start-end>".to_string());
    }
    Answer { lines, found: true }
}

// The first line that reads like a sentence, stopped at the end of that sentence.
fn summary(lines: &[&str]) -> Option<String> {
    let mut in_code = false;
    let start = if matches!(
        lines.first().map(|line| line.trim_end()),
        Some("---" | "+++")
    ) {
        lines
            .iter()
            .skip(1)
            .position(|line| matches!(line.trim_end(), "---" | "+++"))
            .map_or(0, |at| at + 2)
    } else {
        0
    };
    for line in lines.iter().skip(start) {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code = !in_code;
            continue;
        }
        let markup = [
            "#", "|", "<", "![", "[!", ">", "-", "*", "=", ":", "..", "import ", "export ",
        ];
        if in_code || markup.iter().any(|mark| trimmed.starts_with(mark)) {
            continue;
        }
        let text = extract::clean_title(trimmed);
        if text.split_whitespace().count() < 4 {
            continue;
        }
        let sentence = text.split_inclusive(". ").next().unwrap_or(&text).trim();
        return Some(clip(sentence, MAX_SUMMARY));
    }
    None
}

fn format_listing(index: &Index, view: &View) -> Answer {
    let root = Path::new(&index.root);
    let files: Vec<(&str, &FileEntry)> = doc_files(index, view)
        .into_iter()
        .filter(|(_, file)| !file.symbols.is_empty())
        .collect();
    if files.is_empty() {
        return Answer {
            lines: vec!["No documents here".to_string()],
            found: false,
        };
    }

    if files.len() > MAX_LISTED && !view.all && view.limit.is_none() {
        let depth = view
            .within
            .as_deref()
            .map_or(0, |within| within.trim_matches('/').split('/').count());
        let mut folders: BTreeMap<String, (usize, u64)> = BTreeMap::new();
        for (path, file) in &files {
            let parts: Vec<&str> = path.split('/').collect();
            let keep = (depth + 2).min(parts.len() - 1);
            let folder = if keep == 0 {
                ".".to_string()
            } else {
                parts[..keep].join("/")
            };
            let entry = folders.entry(folder).or_default();
            entry.0 += 1;
            entry.1 += u64::from(file.lines);
        }
        let mut lines = vec![format!(
            "{} in {}",
            counted(files.len(), "document"),
            counted(folders.len(), "folder")
        )];
        lines.extend(folders.iter().map(|(folder, (count, total))| {
            format!("{folder}  {}, {total} lines", counted(*count, "document"))
        }));
        lines.push(
            "List a folder with: drift docs --in <folder>. Search with: drift docs <words>"
                .to_string(),
        );
        return Answer { lines, found: true };
    }

    let paths: Vec<&str> = files.iter().map(|(path, _)| *path).collect();
    let dates = last_changed(root, &paths);
    let described: Vec<String> = files
        .par_iter()
        .map(|(path, file)| {
            let source = read_source(root, path).unwrap_or_default();
            let lines: Vec<&str> = source.lines().collect();
            let title = front_matter(&lines, "title")
                .map(str::to_string)
                .or_else(|| {
                    file.symbols
                        .first()
                        .filter(|symbol| symbol.kind != "section")
                        .map(|symbol| symbol.name.clone())
                });
            let about = front_matter(&lines, "description")
                .map(|text| clip(text, MAX_SUMMARY))
                .or_else(|| summary(&lines))
                .filter(|about| title.as_ref() != Some(about));
            let mut line = format!("{path}  {} lines", file.lines);
            if let Some(date) = dates.get(*path) {
                line.push_str(&format!("  ({date})"));
            }
            match (title, about) {
                (Some(title), Some(about)) => {
                    line.push_str(&format!("  # {} - {about}", clip(&title, MAX_LABEL)));
                }
                (Some(text), None) | (None, Some(text)) => {
                    line.push_str(&format!("  # {}", clip(&text, MAX_LABEL + MAX_SUMMARY)));
                }
                (None, None) => {}
            }
            line
        })
        .collect();
    Answer {
        lines: described,
        found: true,
    }
}

pub fn docs(index: &Index, words: &[String], view: &View) -> Answer {
    if words.is_empty() {
        format_listing(index, view)
    } else {
        format_search(index, words, view)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::extract;

    fn stems_match(a: &str, b: &str) -> bool {
        stem(a) == stem(b)
    }

    #[test]
    fn word_forms_share_a_stem() {
        for (a, b) in [
            ("refunds", "refund"),
            ("refunded", "refund"),
            ("configure", "configuring"),
            ("configuration", "configure"),
            ("authentication", "authenticating"),
            ("deployment", "deploy"),
            ("policies", "policy"),
            ("running", "run"),
            ("templates", "template"),
            ("migration", "migrate"),
            ("caching", "cache"),
        ] {
            assert!(stems_match(a, b), "{a} -> {}, {b} -> {}", stem(a), stem(b));
        }
        assert!(!stems_match("status", "state"));
        assert_eq!(stem("class"), "class");
    }

    #[test]
    fn stop_words_are_dropped_unless_nothing_else_is_left() {
        let words = vec!["how do refunds work".to_string()];
        let stems = |words: &[String]| -> Vec<String> {
            query_terms(words)
                .into_iter()
                .map(|(stem, _)| stem)
                .collect()
        };
        assert_eq!(stems(&words), vec!["refund", "work"]);
        assert_eq!(stems(&["how to".to_string()]), vec!["how", "to"]);
        assert_eq!(query_terms(&words)[0].1, "refunds");
    }

    fn sectioned(source: &str) -> Vec<(String, u32, u32)> {
        let extracted = extract("md", source).unwrap();
        let file = FileEntry {
            lang: extracted.lang.to_string(),
            lines: extracted.lines,
            symbols: extracted.symbols,
            imports: Vec::new(),
        };
        let lines: Vec<&str> = source.lines().collect();
        sections(&file, &lines)
            .into_iter()
            .map(|s| (s.title, s.start, s.end))
            .collect()
    }

    #[test]
    fn sections_stop_at_the_next_heading_of_any_level() {
        let source = "intro text\n\n# Top\ntop text\n\n## One\none text\n\n### Deep\ndeep text\n";
        assert_eq!(
            sectioned(source),
            vec![
                ("(start of file)".to_string(), 1, 2),
                ("Top".to_string(), 3, 4),
                ("One".to_string(), 6, 7),
                ("Deep".to_string(), 9, 10),
            ]
        );
    }

    #[test]
    fn a_heading_with_only_subheadings_is_skipped() {
        let source = "# Top\n\n## One\ntext\n\n## Empty\n";
        let titles: Vec<String> = sectioned(source).into_iter().map(|s| s.0).collect();
        assert_eq!(titles, vec!["One", "Empty"]);
    }

    #[test]
    fn long_sections_are_split() {
        let mut source = String::from("# Long\n");
        for _ in 0..30 {
            source.push_str("words\nwords\nwords\nwords\nwords\nwords\nwords\nwords\nwords\n\n");
        }
        let found = sectioned(&source);
        assert!(found.len() > 2, "{found:?}");
        assert!(
            found
                .iter()
                .all(|s| s.0 == "Long" && s.2 - s.1 < LONG_SECTION as u32)
        );
    }

    #[test]
    fn front_matter_gives_a_title() {
        let source = "---\ntitle: \"Guide\"\ndescription: How it works\n---\nSome intro words.\n\n# Part\ntext\n";
        assert_eq!(sectioned(source)[0].0, "Guide");
        let lines: Vec<&str> = source.lines().collect();
        assert_eq!(front_matter(&lines, "description"), Some("How it works"));
        assert_eq!(front_matter(&["# no"], "title"), None);
    }

    #[test]
    fn snippets_show_the_line_with_the_most_matches() {
        let lines = [
            "# Refunds",
            "",
            "Some other line.",
            "A refund policy sets the limit.",
        ];
        let hit = Hit {
            path: "a.md".to_string(),
            title: "Refunds".to_string(),
            trail: Vec::new(),
            start: 1,
            end: 4,
            score: 1.0,
            missing: Vec::new(),
        };
        let terms = vec!["refund".to_string(), "policy".to_string()];
        assert_eq!(
            snippet(&lines, &hit, &terms),
            "A refund policy sets the limit."
        );
    }

    #[test]
    fn changelogs_rank_low_and_licenses_are_left_out() {
        assert_eq!(doc_weight("CHANGELOG.md"), 0.4);
        assert_eq!(doc_weight("tests/fixtures/a.md"), 0.5);
        assert_eq!(doc_weight("docs/guide.md"), 1.0);
        assert!(is_excluded("LICENSE.md"));
        assert!(!is_excluded("docs/licensing.md"));
    }

    #[test]
    fn summaries_skip_markup_and_code() {
        let lines = [
            "---",
            "title: x",
            "---",
            "# Title",
            "![badge](x.png)",
            "```",
            "code that has four words",
            "```",
            "This tool finds things fast. It also does more.",
        ];
        assert_eq!(
            summary(&lines).as_deref(),
            Some("This tool finds things fast.")
        );
    }
}
