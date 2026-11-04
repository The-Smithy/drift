//! The Claude Code hook: a short note when an agent is about to search or read in a way Drift can
//! answer for less. It never blocks a tool, and any failure here means no note.

use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, SystemTime};

use regex::Regex;
use serde_json::{Value, json};

use crate::audit::{pipeline_segments, program_name};
use crate::extract::is_doc_language;
use crate::index::INDEX_FILE;
use crate::install::HOOK_TOOLS;
use crate::model::Index;

// Reads smaller than this are cheap enough that a note would cost more than it saves.
const LARGE_READ_BYTES: u64 = 8 * 1024;
const NAMES_IN_A_NOTE: usize = 3;
const WORDS_IN_A_DOC_SEARCH: usize = 6;
const KEEP_SESSIONS: Duration = Duration::from_secs(7 * 24 * 60 * 60);

// Most searches aren't for a name the index knows, so nothing would ever remind an agent that
// keeps grepping out of habit. This does, on the first such search and then now and again.
const SEARCHES_BETWEEN_REMINDERS: usize = 25;
const SEARCH_LINE: &str = "search";
const REMINDER: &str = "Drift: this repo is indexed. `drift query <name>` finds a definition, `drift refs <name>` its uses, `drift find <words>` searches names and `drift docs <words>` the docs. Grep is right for strings and comments.";

const DOC_EXTENSIONS: &[&str] = &[".md", ".mdx", ".markdown", ".rst", ".adoc", ".asciidoc"];

// Options that take the next word as their value, so it isn't the pattern or a path.
const VALUE_FLAGS: &str = "ABCDdfgmMtT";
const LONG_VALUE_FLAGS: &[&str] = &[
    "--after-context",
    "--before-context",
    "--context",
    "--exclude",
    "--exclude-dir",
    "--file",
    "--max-count",
    "--type-add",
    "--type-not",
];
const LONG_FILTERS: &[&str] = &["--glob", "--include", "--type"];

static DEFINITION_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:(?:export|default|async|pub|public|private|static|abstract|declare)\s+)*(?:function\*?|fn|def|class|const|let|var|type|interface|struct|enum|trait|impl)\s+",
    )
    .expect("valid definition pattern")
});
static IDENTIFIER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_$][A-Za-z0-9_$]{2,}$").expect("valid identifier"));
static PLAIN_WORDS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9 _-]*$").expect("valid words pattern"));

#[derive(Debug, Default, PartialEq)]
struct Search {
    pattern: String,
    fixed: bool,
    ignore_case: bool,
    /// Paths and file filters such as `*.md`. Types are kept as `type:md`.
    targets: Vec<String>,
}

#[derive(Debug, PartialEq)]
enum Ask {
    Search(Search),
    Read(PathBuf),
}

/// Splits one shell command into words, the way a shell would treat quotes.
fn shell_words(segment: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote = None;
    let mut chars = segment.chars().peekable();
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (Some(open), _) if ch == open => quote = None,
            (Some('"'), '\\') if matches!(chars.peek(), Some('"' | '\\')) => {
                word.extend(chars.next());
            }
            (Some(_), _) => word.push(ch),
            (None, '\'' | '"') => {
                quote = Some(ch);
                in_word = true;
            }
            (None, '\\') => {
                // `foo\|bar` and `foo\(` are regex syntax here more often than shell escapes.
                word.push(ch);
                word.extend(chars.next());
                in_word = true;
            }
            (None, _) if ch.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            (None, _) => {
                word.push(ch);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(word);
    }
    words
}

/// The search a grep-like command runs, if this is one.
fn parse_grep(args: &[String], fixed_program: bool) -> Option<Search> {
    let mut search = Search {
        fixed: fixed_program,
        ..Search::default()
    };
    let mut pattern = None;
    let mut positional = Vec::new();
    let mut only_positional = false;
    let mut words = args.iter();
    while let Some(word) = words.next() {
        if only_positional || !word.starts_with('-') || word == "-" {
            positional.push(word.clone());
        } else if word == "--" {
            only_positional = true;
        } else if let Some(long) = word.strip_prefix("--") {
            let (flag, value) = match long.split_once('=') {
                Some((flag, value)) => (format!("--{flag}"), Some(value.to_string())),
                None => (word.clone(), None),
            };
            match flag.as_str() {
                "--ignore-case" => search.ignore_case = true,
                "--fixed-strings" => search.fixed = true,
                "--regexp" => pattern = value.or_else(|| words.next().cloned()),
                _ if LONG_FILTERS.contains(&flag.as_str()) => {
                    if let Some(value) = value.or_else(|| words.next().cloned()) {
                        let typed = flag == "--type";
                        search.targets.push(if typed {
                            format!("type:{value}")
                        } else {
                            value
                        });
                    }
                }
                _ if LONG_VALUE_FLAGS.contains(&flag.as_str()) && value.is_none() => {
                    words.next();
                }
                _ => {}
            }
        } else {
            let cluster = &word[1..];
            for (at, flag) in cluster.char_indices() {
                match flag {
                    'i' => search.ignore_case = true,
                    'F' => search.fixed = true,
                    'e' => {
                        let rest = &cluster[at + 1..];
                        pattern = if rest.is_empty() {
                            words.next().cloned()
                        } else {
                            Some(rest.to_string())
                        };
                        break;
                    }
                    _ if VALUE_FLAGS.contains(flag) => {
                        let rest = &cluster[at + 1..];
                        let value = if rest.is_empty() {
                            words.next().cloned()
                        } else {
                            Some(rest.to_string())
                        };
                        match (flag, value) {
                            ('g', Some(value)) => search.targets.push(value),
                            ('t', Some(value)) => search.targets.push(format!("type:{value}")),
                            _ => {}
                        }
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
    let mut positional = positional.into_iter();
    search.pattern = match pattern {
        Some(pattern) => pattern,
        None => positional.next()?,
    };
    search.targets.extend(positional);
    Some(search)
}

fn parse_select_string(args: &[String]) -> Option<Search> {
    // PowerShell matches without regard to case unless told otherwise.
    let mut search = Search {
        ignore_case: true,
        ..Search::default()
    };
    let mut pattern = None;
    let mut positional = Vec::new();
    let mut words = args.iter();
    while let Some(word) = words.next() {
        match word.to_ascii_lowercase().as_str() {
            "-pattern" => pattern = words.next().cloned(),
            "-path" | "-literalpath" | "-include" => search.targets.extend(words.next().cloned()),
            "-simplematch" => search.fixed = true,
            "-casesensitive" => search.ignore_case = false,
            flag if flag.starts_with('-') => {}
            _ => positional.push(word.clone()),
        }
    }
    let mut positional = positional.into_iter();
    search.pattern = match pattern {
        Some(pattern) => pattern,
        None => positional.next()?,
    };
    search.targets.extend(positional);
    Some(search)
}

fn parse_search(program: &str, words: &[String]) -> Option<Search> {
    let args = &words[1..];
    match program {
        "grep" | "egrep" | "rg" | "ripgrep" => parse_grep(args, false),
        "fgrep" => parse_grep(args, true),
        "git" => {
            let at = args
                .iter()
                .position(|word| !word.starts_with('-'))
                .map(|at| {
                    // `git -C path grep`: the path is the option's value.
                    if at > 0 && args[at - 1] == "-C" {
                        at + 1
                    } else {
                        at
                    }
                })?;
            (args.get(at)? == "grep")
                .then(|| parse_grep(&args[at + 1..], false))
                .flatten()
        }
        "select-string" | "sls" => parse_select_string(args),
        _ => None,
    }
}

/// The directory an argument names. Git Bash on Windows writes `C:\x` as `/c/x`.
fn resolve(dir: &Path, path: &str) -> PathBuf {
    if cfg!(windows) {
        let bytes = path.as_bytes();
        if bytes.len() >= 3
            && bytes[0] == b'/'
            && bytes[1].is_ascii_alphabetic()
            && bytes[2] == b'/'
        {
            return PathBuf::from(format!(
                "{}:{}",
                (bytes[1] as char).to_ascii_uppercase(),
                &path[2..]
            ));
        }
    }
    dir.join(path)
}

/// The files a command prints whole. Empty if it writes to a file or limits what it prints.
fn whole_file_reads(program: &str, words: &[String]) -> Vec<String> {
    if !matches!(program, "cat" | "type" | "get-content" | "gc") {
        return Vec::new();
    }
    let args = &words[1..];
    let limited = args.iter().any(|word| {
        word.starts_with(['<', '>']) || {
            let lower = word.to_ascii_lowercase();
            ["-totalcount", "-tail", "-head", "-first"].contains(&lower.as_str())
        }
    });
    if limited {
        return Vec::new();
    }
    args.iter()
        .filter(|word| !word.starts_with('-'))
        .cloned()
        .collect()
}

fn shell_asks(command: &str, cwd: &Path) -> (Vec<Ask>, PathBuf) {
    let segments = pipeline_segments(command);
    let mut dir = cwd.to_path_buf();
    let mut start = None;
    let mut asks = Vec::new();
    for (at, (segment, piped)) in segments.iter().enumerate() {
        let mut words = shell_words(segment);
        while words
            .first()
            .is_some_and(|word| matches!(word.as_str(), "do" | "then" | "if" | "(" | "{" | "time"))
        {
            words.remove(0);
        }
        let Some(first) = words.first() else { continue };
        let program = program_name(first);
        // Whatever follows a pipe is filtering output, not searching the repo.
        if *piped {
            continue;
        }
        if matches!(program.as_str(), "cd" | "pushd" | "set-location") {
            if let Some(target) = words.get(1) {
                dir = resolve(&dir, target);
            }
            continue;
        }
        if let Some(search) = parse_search(&program, &words) {
            start.get_or_insert_with(|| dir.clone());
            asks.push(Ask::Search(search));
            continue;
        }
        let limited_by_a_pipe = segments.get(at + 1).is_some_and(|(_, piped)| *piped);
        if limited_by_a_pipe {
            continue;
        }
        for file in whole_file_reads(&program, &words) {
            start.get_or_insert_with(|| dir.clone());
            asks.push(Ask::Read(resolve(&dir, &file)));
        }
    }
    (asks, start.unwrap_or(dir))
}

fn grep_tool_ask(input: &Value, cwd: &Path) -> Option<(Vec<Ask>, PathBuf)> {
    let mut search = Search {
        pattern: input["pattern"].as_str()?.to_string(),
        ignore_case: input["-i"].as_bool().unwrap_or(false),
        ..Search::default()
    };
    if let Some(glob) = input["glob"].as_str() {
        search.targets.push(glob.to_string());
    }
    if let Some(file_type) = input["type"].as_str() {
        search.targets.push(format!("type:{file_type}"));
    }
    let mut start = cwd.to_path_buf();
    if let Some(path) = input["path"].as_str() {
        search.targets.push(path.to_string());
        start = resolve(cwd, path);
    }
    Some((vec![Ask::Search(search)], start))
}

fn asks_for(tool: &str, input: &Value, cwd: &Path) -> Option<(Vec<Ask>, PathBuf)> {
    match tool {
        "Grep" => grep_tool_ask(input, cwd),
        "Read" => {
            if !input["offset"].is_null() || !input["limit"].is_null() {
                return None;
            }
            let file = resolve(cwd, input["file_path"].as_str()?);
            let start = file.parent()?.to_path_buf();
            Some((vec![Ask::Read(file)], start))
        }
        "Bash" | "PowerShell" => Some(shell_asks(input["command"].as_str()?, cwd)),
        _ => None,
    }
}

/// The alternatives in a pattern: `foo|bar` and `foo\|bar` both give `foo` and `bar`.
fn alternatives(search: &Search) -> Vec<String> {
    if search.fixed {
        return vec![search.pattern.clone()];
    }
    search
        .pattern
        .replace("\\|", "|")
        .split('|')
        .map(str::to_string)
        .collect()
}

/// The identifier an alternative looks for, allowing for what agents wrap around one:
/// `\bname\b`, `name\(`, `function\s+name`, `(name`.
fn searched_identifier(alternative: &str) -> Option<String> {
    let mut text = alternative
        .replace("\\s+", " ")
        .replace("\\s*", " ")
        .replace("\\s", " ")
        .replace("\\b", "");
    text = text
        .trim_start_matches(['^', '(', '\\', '<', ' ', '"', '\''])
        .trim_end_matches(['$', ')', '(', '\\', '>', ' ', ':', '=', '<', '"', '\''])
        .to_string();
    if let Some(found) = DEFINITION_WORDS.find(&text) {
        text = text[found.end()..].to_string();
    }
    IDENTIFIER.is_match(&text).then_some(text)
}

// A lowercase word in a longer pattern or a case-insensitive search is usually text, even when
// something in the code happens to have that name. Mixed case or an underscore means a name.
fn looks_like_a_name(term: &str, ignore_case: bool, alone: bool) -> bool {
    let styled = term.contains('_') || term.chars().any(|c| c.is_ascii_uppercase());
    styled || (alone && !ignore_case)
}

fn is_doc_path(target: &str) -> bool {
    let lower = target.to_ascii_lowercase();
    DOC_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
}

fn is_wildcard(target: &str) -> bool {
    target.contains(['*', '?', '{'])
}

/// Whether a search covers documents across a folder. One named file is a text search of it.
fn searches_docs(search: &Search) -> bool {
    search.targets.iter().any(|target| {
        let lower = target.to_ascii_lowercase();
        if lower == "type:md" || lower == "type:markdown" {
            return true;
        }
        if is_doc_path(&lower) {
            return is_wildcard(&lower);
        }
        let last = lower
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next();
        matches!(last, Some("docs" | "doc" | "documentation"))
    })
}

fn only_named_docs(search: &Search) -> bool {
    !search.targets.is_empty()
        && search
            .targets
            .iter()
            .all(|target| is_doc_path(target) && !is_wildcard(target))
}

fn places(index: &Index, name: &str) -> Vec<String> {
    index
        .files
        .iter()
        .flat_map(|(path, file)| {
            file.symbols
                .iter()
                .filter(|symbol| {
                    symbol.name == name
                        && !matches!(symbol.kind.as_str(), "heading" | "key" | "section")
                })
                .map(move |symbol| format!("{path}:{}", symbol.line))
        })
        .collect()
}

fn has_name_containing(index: &Index, part: &str) -> bool {
    index.files.values().any(|file| {
        file.symbols.iter().any(|symbol| {
            symbol.name.contains(part)
                && !matches!(symbol.kind.as_str(), "heading" | "key" | "section")
        })
    })
}

fn where_defined(name: &str, mut found: Vec<String>) -> String {
    let total = found.len();
    found.truncate(2);
    let more = if total > 2 {
        format!(" and {} more", total - 2)
    } else {
        String::new()
    };
    format!("{name} is defined at {}{more}", found.join(", "))
}

/// A note with the key that stops it repeating in a session.
type Note = (String, String);

fn docs_note(index: &Index, search: &Search) -> Option<Note> {
    let has_docs = index.files.values().any(|file| is_doc_language(&file.lang));
    if !has_docs {
        return None;
    }
    let mut seen = HashSet::new();
    let words: Vec<String> = alternatives(search)
        .into_iter()
        .map(|alternative| alternative.trim().to_string())
        .filter(|alternative| PLAIN_WORDS.is_match(alternative))
        .filter(|alternative| seen.insert(alternative.to_ascii_lowercase()))
        .take(WORDS_IN_A_DOC_SEARCH)
        .collect();
    if words.is_empty() {
        return None;
    }
    Some((
        "docs".to_string(),
        format!(
            "Drift: `drift docs {}` ranks the doc sections that cover this, with their line ranges.",
            words.join(" ")
        ),
    ))
}

fn search_notes(index: &Index, search: &Search) -> Vec<Note> {
    // Dependencies aren't indexed, so what's defined in the repo doesn't answer this search.
    if search
        .targets
        .iter()
        .any(|target| target.contains("node_modules"))
    {
        return Vec::new();
    }
    if searches_docs(search) {
        return docs_note(index, search).into_iter().collect();
    }
    if only_named_docs(search) {
        return Vec::new();
    }
    let alternatives = alternatives(search);
    let alone = alternatives.len() == 1;
    let mut seen = HashSet::new();
    let terms: Vec<String> = alternatives
        .iter()
        .filter_map(|alternative| searched_identifier(alternative))
        .filter(|term| looks_like_a_name(term, search.ignore_case, alone))
        .filter(|term| seen.insert(term.clone()))
        .collect();

    let mut notes = Vec::new();
    for term in terms {
        let found = places(index, &term);
        if !found.is_empty() {
            notes.push((format!("name:{term}"), (term, found)));
        }
    }
    let defined: Vec<&str> = notes.iter().map(|(_, (term, _))| term.as_str()).collect();
    if !defined.is_empty() {
        return defined_notes(&notes);
    }
    // Part of a name, such as `formField` for `formFieldSchema`. A plain word is too likely to be text.
    alternatives
        .iter()
        .filter_map(|alternative| searched_identifier(alternative))
        .filter(|term| term.contains('_') || term.chars().skip(1).any(|c| c.is_ascii_uppercase()))
        .find(|term| has_name_containing(index, term))
        .map(|term| {
            (
                format!("find:{term}"),
                format!(
                    "Drift: `drift find {term}` lists the definitions with this in their name."
                ),
            )
        })
        .into_iter()
        .collect()
}

fn defined_notes(found: &[(String, (String, Vec<String>))]) -> Vec<Note> {
    let listed = &found[..found.len().min(NAMES_IN_A_NOTE)];
    listed
        .iter()
        .map(|(key, (term, places))| {
            let hint = if listed.len() == 1 {
                format!("`drift show {term}` prints it, `drift refs {term}` lists its uses.")
            } else {
                format!("`drift show {term}` prints it.")
            };
            (
                key.clone(),
                format!("Drift: {}. {hint}", where_defined(term, places.clone())),
            )
        })
        .collect()
}

fn read_note(index: &Index, root: &Path, file_path: &Path) -> Option<Note> {
    let size = fs::metadata(file_path).ok()?.len();
    if size < LARGE_READ_BYTES {
        return None;
    }
    let canonical = file_path.canonicalize().ok()?;
    let canonical_root = root.canonicalize().ok()?;
    let relative = canonical
        .strip_prefix(canonical_root)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    let file = index.files.get(&relative)?;
    let kb = size / 1024;
    let text = if is_doc_language(&file.lang) {
        format!(
            "Drift: this file is {kb} KB. `drift outline {relative}` lists its sections with line ranges, and `drift docs <words>` finds the right one."
        )
    } else {
        format!(
            "Drift: this file is {kb} KB. `drift outline {relative}` lists its definitions with line ranges, so you can read only what you need."
        )
    };
    Some((format!("read:{relative}"), text))
}

fn seen_file(session: &str) -> Option<PathBuf> {
    let safe: String = session
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    (!safe.is_empty()).then(|| std::env::temp_dir().join("drift-hook").join(safe))
}

fn forget_old_sessions(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > KEEP_SESSIONS);
        if old {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Keeps the notes this session hasn't had yet, and records them. Repeating a note costs tokens
/// and says nothing new. If the record can't be used, every note goes through.
fn new_this_session(session: Option<&str>, notes: Vec<Note>) -> Vec<Note> {
    let Some(path) = session.and_then(seen_file) else {
        return notes;
    };
    let existing = fs::read_to_string(&path).ok();
    let seen: HashSet<&str> = existing.as_deref().unwrap_or("").lines().collect();
    let fresh: Vec<Note> = notes
        .into_iter()
        .filter(|(key, _)| !seen.contains(key.as_str()))
        .collect();
    if fresh.is_empty() {
        return fresh;
    }
    if let Some(dir) = path.parent()
        && fs::create_dir_all(dir).is_ok()
        && existing.is_none()
    {
        forget_old_sessions(dir);
    }
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        for (key, _) in &fresh {
            let _ = writeln!(file, "{key}");
        }
    }
    fresh
}

/// Counts a search that earned no note, and says whether it's time for the reminder.
fn reminder_due(session: Option<&str>) -> bool {
    let Some(path) = session.and_then(seen_file) else {
        return false;
    };
    let earlier = fs::read_to_string(&path)
        .map(|text| text.lines().filter(|line| *line == SEARCH_LINE).count())
        .unwrap_or(0);
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) else {
        return false;
    };
    writeln!(file, "{SEARCH_LINE}").is_ok() && earlier % SEARCHES_BETWEEN_REMINDERS == 0
}

fn note_for(event: &Value) -> Option<String> {
    let tool = event["tool_name"].as_str()?;
    if !HOOK_TOOLS.contains(&tool) {
        return None;
    }
    let cwd = PathBuf::from(event["cwd"].as_str()?);
    // Work out what's being asked before loading the index, since most commands ask nothing.
    let (asks, start) = asks_for(tool, &event["tool_input"], &cwd)?;
    if asks.is_empty() {
        return None;
    }
    let root = start
        .ancestors()
        .find(|dir| dir.join(INDEX_FILE).exists())?
        .to_path_buf();
    let text = fs::read_to_string(root.join(INDEX_FILE)).ok()?;
    let index: Index = serde_json::from_str(&text).ok()?;

    let mut notes = Vec::new();
    for ask in &asks {
        match ask {
            Ask::Search(search) => notes.extend(search_notes(&index, search)),
            Ask::Read(file) => notes.extend(read_note(&index, &root, file)),
        }
    }
    let mut keys = HashSet::new();
    notes.retain(|(key, _)| keys.insert(key.clone()));
    let session = event["session_id"].as_str();
    if notes.is_empty() {
        let searched = asks.iter().any(|ask| matches!(ask, Ask::Search(_)));
        return (searched && reminder_due(session)).then(|| REMINDER.to_string());
    }
    let notes = new_this_session(session, notes);
    if notes.is_empty() {
        return None;
    }
    let texts: Vec<String> = notes.into_iter().map(|(_, text)| text).collect();
    Some(texts.join("\n"))
}

// Must never get in the agent's way: any failure here means no note, and the tool runs as normal.
pub fn run() {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return;
    }
    let Ok(event) = serde_json::from_str::<Value>(&input) else {
        return;
    };
    if let Some(note) = note_for(&event) {
        let output = json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "additionalContext": note,
            }
        });
        println!("{output}");
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::*;

    fn searches(command: &str) -> Vec<Search> {
        shell_asks(command, Path::new("."))
            .0
            .into_iter()
            .filter_map(|ask| match ask {
                Ask::Search(search) => Some(search),
                Ask::Read(_) => None,
            })
            .collect()
    }

    fn patterns(command: &str) -> Vec<String> {
        searches(command).into_iter().map(|s| s.pattern).collect()
    }

    fn identifiers(search: &Search) -> Vec<String> {
        alternatives(search)
            .iter()
            .filter_map(|alternative| searched_identifier(alternative))
            .collect()
    }

    #[test]
    fn searches_are_found_wherever_they_sit_in_a_command() {
        assert_eq!(patterns("grep -rn loadUser src/"), ["loadUser"]);
        assert_eq!(patterns("rg 'loadUser'"), ["loadUser"]);
        assert_eq!(patterns("git grep -n loadUser"), ["loadUser"]);
        assert_eq!(patterns("git -C ../other grep loadUser"), ["loadUser"]);
        assert_eq!(patterns("cd src && rg loadUser"), ["loadUser"]);
        assert_eq!(
            patterns("cd /c/x; grep -n a1 f; echo ---; grep -n b2 g"),
            ["a1", "b2"]
        );
        assert_eq!(
            patterns("for f in a b; do grep -n name1 $f; done"),
            ["name1"]
        );
        assert_eq!(
            patterns("Select-String -Path a.ts -Pattern loadUser"),
            ["loadUser"]
        );
    }

    #[test]
    fn a_grep_after_a_pipe_only_filters_output() {
        assert!(patterns("cargo test | grep failed").is_empty());
        assert!(patterns("git log | grep -i fix | head").is_empty());
        assert_eq!(patterns("grep -n a1 f | head -5"), ["a1"]);
    }

    #[test]
    fn option_values_are_not_taken_for_the_pattern() {
        assert_eq!(patterns("grep -A 5 -n loadUser src"), ["loadUser"]);
        assert_eq!(patterns("grep -n -e loadUser src"), ["loadUser"]);
        assert_eq!(patterns("grep -rne loadUser src"), ["loadUser"]);
        assert_eq!(patterns("grep --max-count 3 loadUser src"), ["loadUser"]);
        assert_eq!(patterns("grep --include='*.ts' loadUser src"), ["loadUser"]);
        assert_eq!(patterns("rg -g '*.ts' -t ts loadUser"), ["loadUser"]);
        let found = &searches("rg -g '*.md' -i refunds docs")[0];
        assert!(found.ignore_case);
        assert_eq!(found.targets, ["*.md", "docs"]);
    }

    #[test]
    fn identifiers_are_found_in_what_agents_wrap_around_them() {
        for pattern in [
            "loadUser",
            "\\bloadUser\\b",
            "loadUser\\(",
            "function loadUser",
            "export const loadUser",
            "function\\s+loadUser",
            "^loadUser",
            "(loadUser",
            "loadUser =",
        ] {
            let search = Search {
                pattern: pattern.to_string(),
                ..Search::default()
            };
            assert_eq!(identifiers(&search), ["loadUser"], "{pattern}");
        }
    }

    #[test]
    fn alternatives_are_split_unless_the_search_is_fixed() {
        for pattern in [
            "loadUser|saveUser",
            "loadUser\\|saveUser",
            "(loadUser|saveUser)",
        ] {
            let search = Search {
                pattern: pattern.to_string(),
                ..Search::default()
            };
            assert_eq!(identifiers(&search), ["loadUser", "saveUser"], "{pattern}");
        }
        let fixed = Search {
            pattern: "a|b".to_string(),
            fixed: true,
            ..Search::default()
        };
        assert_eq!(alternatives(&fixed), ["a|b"]);
        let regex = Search {
            pattern: "load.*User".to_string(),
            ..Search::default()
        };
        assert!(identifiers(&regex).is_empty());
    }

    #[test]
    fn plain_words_in_longer_or_loose_searches_count_as_text() {
        assert!(looks_like_a_name("loadUser", true, false));
        assert!(looks_like_a_name("load_user", false, false));
        assert!(looks_like_a_name("Netlify", false, false));
        assert!(looks_like_a_name("blocks", false, true));
        assert!(!looks_like_a_name("blocks", false, false));
        assert!(!looks_like_a_name("blocks", true, true));
    }

    #[test]
    fn doc_searches_cover_a_folder_and_not_one_file() {
        let search = |targets: &[&str]| Search {
            pattern: "x".to_string(),
            targets: targets.iter().map(|target| target.to_string()).collect(),
            ..Search::default()
        };
        assert!(searches_docs(&search(&["docs/"])));
        assert!(searches_docs(&search(&["**/*.md"])));
        assert!(searches_docs(&search(&["type:md"])));
        assert!(!searches_docs(&search(&["README.md"])));
        assert!(!searches_docs(&search(&["src"])));
        assert!(only_named_docs(&search(&["README.md", "AGENTS.md"])));
        assert!(!only_named_docs(&search(&["README.md", "src"])));
        assert!(!only_named_docs(&search(&[])));
    }

    #[test]
    fn printing_a_whole_file_counts_as_reading_it() {
        let reads = |command: &str| {
            shell_asks(command, Path::new("/repo"))
                .0
                .into_iter()
                .filter(|ask| matches!(ask, Ask::Read(_)))
                .count()
        };
        assert_eq!(reads("cat src/a.ts"), 1);
        assert_eq!(reads("cd x && cat a.ts b.ts"), 2);
        assert_eq!(reads("Get-Content a.ts"), 1);
        assert_eq!(reads("cat a.ts | head -5"), 0);
        assert_eq!(reads("cat > a.ts"), 0);
        assert_eq!(reads("cat <<EOF"), 0);
        assert_eq!(reads("Get-Content a.ts -TotalCount 5"), 0);
        assert_eq!(reads("sed -n 1,9p a.ts"), 0);
    }

    #[test]
    fn cd_moves_where_the_index_is_looked_for() {
        let (_, start) = shell_asks("cd sub && grep -n a1 f", Path::new("/repo"));
        assert_eq!(start, Path::new("/repo").join("sub"));
    }

    #[test]
    fn other_tools_and_ranged_reads_ask_nothing() {
        let cwd = Path::new(".");
        assert!(asks_for("Edit", &json!({}), cwd).is_none());
        assert!(asks_for("Read", &json!({ "file_path": "a", "limit": 5 }), cwd).is_none());
        assert!(asks_for("Grep", &json!({}), cwd).is_none());
        assert!(
            asks_for("Bash", &json!({ "command": "ls" }), cwd)
                .unwrap()
                .0
                .is_empty()
        );
    }

    #[test]
    fn a_note_is_only_given_once_a_session() {
        let session = format!("drift-test-{}", std::process::id());
        let note = || vec![("name:a".to_string(), "a note".to_string())];
        assert_eq!(new_this_session(Some(&session), note()).len(), 1);
        assert!(new_this_session(Some(&session), note()).is_empty());
        assert_eq!(new_this_session(None, note()).len(), 1);
        let _ = std::fs::remove_file(seen_file(&session).unwrap());
    }
}
