//! `drift audit`: where agent tokens went, from local Claude Code transcripts.
//!
//! This prints totals only, never file names or contents, and sends nothing anywhere.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Result, ensure};
use rayon::prelude::*;
use serde_json::{Value, json};

use crate::extract::is_supported;

const BYTES_PER_TOKEN: u64 = 4;
const DAY: u64 = 24 * 60 * 60;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
enum Activity {
    Reading,
    Searching,
    Listing,
    Building,
    Git,
    Drift,
    Other,
}

impl Activity {
    fn label(self) -> &'static str {
        match self {
            Activity::Reading => "Reading files",
            Activity::Searching => "Searching files",
            Activity::Listing => "Listing files",
            Activity::Building => "Building, testing and running",
            Activity::Git => "git",
            Activity::Drift => "Drift",
            Activity::Other => "Everything else",
        }
    }
}

// Calls Drift could answer, the question this report exists for.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Finding {
    DriftLookup,
    NameSearch,
    WholeFileRead,
}

#[derive(Clone, Copy, PartialEq, Debug)]
struct Call {
    activity: Activity,
    finding: Option<Finding>,
}

#[derive(Default, Clone, Copy)]
struct Totals {
    calls: u64,
    bytes: u64,
}

impl Totals {
    fn add(&mut self, bytes: u64) {
        self.calls += 1;
        self.bytes += bytes;
    }

    fn tokens(self) -> u64 {
        self.bytes / BYTES_PER_TOKEN
    }

    fn average_tokens(self) -> u64 {
        self.tokens().checked_div(self.calls).unwrap_or(0)
    }
}

const DRIFT_LOOKUPS: &[&str] = &[
    "query",
    "find",
    "docs",
    "show",
    "sig",
    "outline",
    "refs",
    "tests",
    "importers",
    "context",
    "map",
    "changes",
];

const SETUP_COMMANDS: &[&str] = &[
    "cd",
    "set",
    "export",
    "source",
    "mkdir",
    "pwd",
    "true",
    "set-location",
    "echo",
    "rm",
    "for",
    "while",
    "if",
    "else",
    "fi",
    "done",
];
// `do` and `then` start the real command in `for x in y; do grep z; done`.
const PREFIXES: &[&str] = &[
    "sudo", "time", "timeout", "env", "command", "exec", "do", "then", "corepack",
];
const READERS: &[&str] = &[
    "cat",
    "head",
    "tail",
    "sed",
    "awk",
    "less",
    "more",
    "bat",
    "nl",
    "type",
    "get-content",
    "gc",
];
const SEARCHERS: &[&str] = &[
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "ag",
    "ack",
    "select-string",
    "sls",
];
const LISTERS: &[&str] = &["ls", "find", "fd", "tree", "dir", "get-childitem", "gci"];
const BUILDERS: &[&str] = &[
    "cargo",
    "npm",
    "pnpm",
    "yarn",
    "npx",
    "bun",
    "deno",
    "node",
    "python",
    "python3",
    "py",
    "pytest",
    "uv",
    "pip",
    "go",
    "make",
    "cmake",
    "rustc",
    "tsc",
    "vitest",
    "jest",
    "playwright",
    "eslint",
    "ruff",
    "dotnet",
    "godot",
    "bash",
    "sh",
];

/// Splits a shell command at `&&`, `||`, `;`, `|` and newlines, outside quotes.
fn segments(command: &str) -> Vec<&str> {
    pipeline_segments(command)
        .into_iter()
        .map(|(segment, _)| segment)
        .collect()
}

/// The same parts, each with whether a pipe feeds it.
pub(crate) fn pipeline_segments(command: &str) -> Vec<(&str, bool)> {
    let mut parts = Vec::new();
    let mut quote = None;
    let mut start = 0;
    let mut piped = false;
    let mut chars = command.char_indices().peekable();
    while let Some((at, ch)) = chars.next() {
        match (quote, ch) {
            (Some(open), _) if ch == open => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(ch),
            (None, ';' | '\n') => {
                parts.push((&command[start..at], piped));
                start = at + ch.len_utf8();
                piped = false;
            }
            (None, '&' | '|') => {
                parts.push((&command[start..at], piped));
                start = at + 1;
                let doubled = chars.peek().is_some_and(|(_, next)| *next == ch);
                if doubled {
                    chars.next();
                    start = at + 2;
                }
                piped = ch == '|' && !doubled;
            }
            _ => {}
        }
    }
    parts.push((&command[start..], piped));
    parts
}

fn unquote(word: &str) -> &str {
    word.trim_matches(|c| c == '"' || c == '\'')
}

pub(crate) fn program_name(word: &str) -> String {
    let word = unquote(word);
    let base = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let lower = base.to_ascii_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
}

/// The words of the first part of a command that produces output. In `cd x && sed -n 1,9p f | grep y`
/// that's `sed`: most of the output came from reading the file, and `grep` only cut it down.
fn first_command(command: &str) -> Vec<&str> {
    for segment in segments(command) {
        let mut words: Vec<&str> = segment.split_whitespace().collect();
        while let Some(first) = words.first() {
            let is_prefix = PREFIXES.contains(&program_name(first).as_str());
            let assigns = first.contains('=') && !first.starts_with('-');
            // The 60 in `timeout 60 cargo test`.
            let is_number = first.trim_end_matches('s').parse::<u64>().is_ok();
            if !is_prefix && !assigns && !is_number {
                break;
            }
            words.remove(0);
        }
        let Some(first) = words.first() else { continue };
        let program = program_name(first);
        if !SETUP_COMMANDS.contains(&program.as_str()) && !program.starts_with("$env") {
            return words;
        }
    }
    Vec::new()
}

/// A plain identifier that Drift could look up, allowing for `\b` around it.
fn is_name(pattern: &str) -> bool {
    let pattern = unquote(pattern);
    let pattern = pattern.strip_prefix("\\b").unwrap_or(pattern);
    let pattern = pattern.strip_suffix("\\b").unwrap_or(pattern);
    pattern.len() >= 3
        && pattern.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && pattern
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_code_path(path: &str) -> bool {
    Path::new(unquote(path))
        .extension()
        .and_then(|extension| extension.to_str())
        // Plain text is only sometimes a document, so reading one isn't clearly a waste.
        .is_some_and(|extension| is_supported(extension) && extension != "txt")
}

fn searched_for_a_name(words: &[&str]) -> bool {
    words
        .iter()
        .skip(1)
        .find(|word| !word.starts_with('-') && word.parse::<u64>().is_err())
        .is_some_and(|pattern| is_name(pattern))
}

// A Drift lookup counts wherever it sits in a command, since this report exists to count them.
fn runs_drift_lookup(command: &str) -> bool {
    segments(command).iter().any(|segment| {
        let mut words = segment
            .split_whitespace()
            .map(|word| word.trim_matches(|c| c == '(' || c == ')'))
            .skip_while(|word| matches!(*word, "do" | "then" | ""));
        words
            .next()
            .is_some_and(|word| program_name(word) == "drift")
            && words.next().is_some_and(|sub| DRIFT_LOOKUPS.contains(&sub))
    })
}

fn classify_shell(command: &str) -> Call {
    if runs_drift_lookup(command) {
        return Call {
            activity: Activity::Drift,
            finding: Some(Finding::DriftLookup),
        };
    }
    let words = first_command(command);
    let Some(first) = words.first() else {
        return Call {
            activity: Activity::Other,
            finding: None,
        };
    };
    let program = program_name(first);
    let program = program.as_str();
    let (activity, finding) = if program == "drift" {
        let lookup = words.get(1).is_some_and(|sub| DRIFT_LOOKUPS.contains(sub));
        (Activity::Drift, lookup.then_some(Finding::DriftLookup))
    } else if program == "git" && words.get(1) == Some(&"grep") {
        (
            Activity::Searching,
            searched_for_a_name(&words[1..]).then_some(Finding::NameSearch),
        )
    } else if program == "git" || program == "gh" || program == "$gh" {
        (Activity::Git, None)
    } else if READERS.contains(&program) {
        let whole_file = (program == "cat" || program == "type" || program == "get-content")
            && words.iter().skip(1).any(|word| is_code_path(word));
        (
            Activity::Reading,
            whole_file.then_some(Finding::WholeFileRead),
        )
    } else if SEARCHERS.contains(&program) {
        (
            Activity::Searching,
            searched_for_a_name(&words).then_some(Finding::NameSearch),
        )
    } else if LISTERS.contains(&program) {
        (Activity::Listing, None)
    } else if BUILDERS.contains(&program) {
        (Activity::Building, None)
    } else {
        (Activity::Other, None)
    };
    Call { activity, finding }
}

fn classify(name: &str, input: &Value) -> Call {
    let text = |key: &str| input.get(key).and_then(Value::as_str).unwrap_or_default();
    match name {
        "Read" => {
            let whole = input.get("offset").is_none() && input.get("limit").is_none();
            Call {
                activity: Activity::Reading,
                finding: (whole && is_code_path(text("file_path")))
                    .then_some(Finding::WholeFileRead),
            }
        }
        "Grep" => Call {
            activity: Activity::Searching,
            finding: is_name(text("pattern")).then_some(Finding::NameSearch),
        },
        "Glob" => Call {
            activity: Activity::Listing,
            finding: None,
        },
        "Bash" | "PowerShell" => classify_shell(text("command")),
        _ => Call {
            activity: Activity::Other,
            finding: None,
        },
    }
}

// Images are left out: they're counted differently and would swamp everything else.
fn result_bytes(content: &Value) -> u64 {
    match content {
        Value::String(text) => text.len() as u64,
        Value::Array(items) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .map(|text| text.len() as u64)
            .sum(),
        _ => 0,
    }
}

struct Seen {
    id: String,
    call: Call,
    bytes: u64,
}

fn read_transcript(path: &Path, since: &str) -> Vec<Seen> {
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    let mut calls: HashMap<String, Call> = HashMap::new();
    let mut seen = Vec::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        // These hold whole API requests, which repeat what the other records say.
        if line.contains("\"type\":\"api-request") {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let kind = record.get("type").and_then(Value::as_str);
        if kind != Some("user") && kind != Some("assistant") {
            continue;
        }
        let Some(blocks) = record.pointer("/message/content").and_then(Value::as_array) else {
            continue;
        };
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("tool_use") => {
                    let id = block.get("id").and_then(Value::as_str);
                    let name = block.get("name").and_then(Value::as_str);
                    if let (Some(id), Some(name)) = (id, name) {
                        let input = block.get("input").unwrap_or(&Value::Null);
                        calls.insert(id.to_string(), classify(name, input));
                    }
                }
                Some("tool_result") => {
                    let recent = record
                        .get("timestamp")
                        .and_then(Value::as_str)
                        .is_some_and(|time| time >= since);
                    let id = block.get("tool_use_id").and_then(Value::as_str);
                    if let (true, Some(id)) = (recent, id)
                        && let Some(call) = calls.get(id)
                    {
                        seen.push(Seen {
                            id: id.to_string(),
                            call: *call,
                            bytes: block.get("content").map_or(0, result_bytes),
                        });
                    }
                }
                _ => {}
            }
        }
    }
    seen
}

fn transcripts(dir: &Path, project: Option<&str>, oldest: SystemTime) -> Vec<PathBuf> {
    fn walk(dir: &Path, oldest: SystemTime, found: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, oldest, found);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
                && entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .is_ok_and(|modified| modified >= oldest)
            {
                found.push(path);
            }
        }
    }

    let wanted = project.map(str::to_ascii_lowercase);
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if wanted.as_deref().is_none_or(|text| name.contains(text)) {
            walk(&entry.path(), oldest, &mut found);
        }
    }
    found
}

fn default_dir() -> Option<PathBuf> {
    let environment = |name| std::env::var_os(name).filter(|value| !value.is_empty());
    let config = environment("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            environment("USERPROFILE")
                .or_else(|| environment("HOME"))
                .map(|home| Path::new(&home).join(".claude"))
        })?;
    Some(config.join("projects"))
}

struct Report {
    days: u64,
    results: Totals,
    by_activity: Vec<(Activity, Totals)>,
    drift_lookups: Totals,
    name_searches: Totals,
    whole_file_reads: Totals,
}

fn summarize(seen: Vec<Seen>, days: u64) -> Report {
    // A resumed session repeats earlier results in its new file, so each call counts once.
    let mut unique: HashMap<String, (Call, u64)> = HashMap::new();
    for item in seen {
        unique.insert(item.id, (item.call, item.bytes));
    }

    let mut results = Totals::default();
    let mut by_activity: HashMap<Activity, Totals> = HashMap::new();
    let (mut drift_lookups, mut name_searches, mut whole_file_reads) =
        (Totals::default(), Totals::default(), Totals::default());
    for (call, bytes) in unique.into_values() {
        results.add(bytes);
        by_activity.entry(call.activity).or_default().add(bytes);
        match call.finding {
            Some(Finding::DriftLookup) => drift_lookups.add(bytes),
            Some(Finding::NameSearch) => name_searches.add(bytes),
            Some(Finding::WholeFileRead) => whole_file_reads.add(bytes),
            None => {}
        }
    }
    let mut by_activity: Vec<_> = by_activity.into_iter().collect();
    by_activity.sort_by_key(|(activity, totals)| (std::cmp::Reverse(totals.bytes), *activity));
    Report {
        days,
        results,
        by_activity,
        drift_lookups,
        name_searches,
        whole_file_reads,
    }
}

fn short(tokens: u64) -> String {
    match tokens {
        0..1_000 => tokens.to_string(),
        1_000..1_000_000 => format!("{:.1}K", tokens as f64 / 1e3),
        _ => format!("{:.1}M", tokens as f64 / 1e6),
    }
}

fn plural(count: u64, word: &str) -> String {
    format!("{count} {word}{}", if count == 1 { "" } else { "s" })
}

fn percent(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 * 100.0 / whole as f64
    }
}

fn print_report(report: &Report) {
    println!(
        "Last {}: {}, about {} tokens of tool output",
        plural(report.days, "day"),
        plural(report.results.calls, "tool result"),
        short(report.results.tokens())
    );
    println!("Tool output only, at 4 bytes per token. Images and resent context aren't counted.");
    println!("\nBy activity:");
    for (activity, totals) in &report.by_activity {
        println!(
            "  {:30} {:>3.0}%  {:>7} tokens  {} calls",
            activity.label(),
            percent(totals.bytes, report.results.bytes),
            short(totals.tokens()),
            totals.calls
        );
    }
    println!("\nFinding code:");
    for (label, totals) in [
        ("Drift lookups", report.drift_lookups),
        ("Searches for one name", report.name_searches),
        ("Whole-file reads", report.whole_file_reads),
    ] {
        println!(
            "  {label:30} {:>6} calls  {:>7} tokens  {:>6} per call",
            totals.calls,
            short(totals.tokens()),
            short(totals.average_tokens())
        );
    }
    let drift = report.drift_lookups.calls;
    let total = drift + report.name_searches.calls + report.whole_file_reads.calls;
    println!(
        "Drift handled {:.0}% of these calls.",
        percent(drift, total)
    );
}

fn report_json(report: &Report) -> Value {
    let totals = |totals: Totals| json!({"calls": totals.calls, "tokens": totals.tokens()});
    json!({
        "days": report.days,
        "results": totals(report.results),
        "by_activity": report.by_activity.iter().map(|(activity, value)| {
            json!({"activity": activity.label(), "calls": value.calls, "tokens": value.tokens()})
        }).collect::<Vec<_>>(),
        "finding_code": {
            "drift_lookups": totals(report.drift_lookups),
            "name_searches": totals(report.name_searches),
            "whole_file_reads": totals(report.whole_file_reads),
        },
    })
}

pub fn run(days: u64, project: Option<&str>, dir: Option<&Path>, as_json: bool) -> Result<()> {
    let dir = match dir {
        Some(dir) => dir.to_path_buf(),
        None => default_dir().ok_or_else(|| anyhow::anyhow!("Couldn't find your home folder"))?,
    };
    ensure!(
        dir.is_dir(),
        "No Claude Code transcripts in {}. Use --dir to say where they are.",
        dir.display()
    );

    let since = SystemTime::now() - Duration::from_secs(days * DAY);
    let cutoff = humantime::format_rfc3339_seconds(since).to_string();
    let files = transcripts(&dir, project, since);
    let seen: Vec<Seen> = files
        .par_iter()
        .flat_map_iter(|file| read_transcript(file, &cutoff))
        .collect();
    let report = summarize(seen, days);
    ensure!(
        report.results.calls > 0,
        "No tool results in the last {} in {}.",
        plural(days, "day"),
        dir.display()
    );

    if as_json {
        println!("{}", serde_json::to_string_pretty(&report_json(&report))?);
    } else {
        print_report(&report);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(command: &str) -> Call {
        classify_shell(command)
    }

    #[test]
    fn credits_a_chain_to_the_command_that_made_the_output() {
        assert_eq!(
            shell("sed -n 1,50p src/a.ts | grep foo").activity,
            Activity::Reading
        );
        assert_eq!(shell("git log | grep foo").activity, Activity::Git);
        assert_eq!(
            shell("cd src && grep -rn foo .").activity,
            Activity::Searching
        );
        assert_eq!(shell("cat a.txt | wc -l").activity, Activity::Reading);
        assert_eq!(shell("FOO=1 cargo test").activity, Activity::Building);
        assert_eq!(shell("cd /c/x; pnpm build").activity, Activity::Building);
        assert_eq!(shell("timeout 60 cargo test").activity, Activity::Building);
        assert_eq!(shell("corepack pnpm test").activity, Activity::Building);
        assert_eq!(shell(r#"S="/c/x"; cat "$S/a""#).activity, Activity::Reading);
        assert_eq!(
            shell("echo start; grep -r x .").activity,
            Activity::Searching
        );
    }

    #[test]
    fn looks_inside_loops() {
        let call = shell("for d in a b; do drift map; done");
        assert_eq!(call.activity, Activity::Drift);
        assert_eq!(call.finding, Some(Finding::DriftLookup));
        assert_eq!(
            shell(
                "for f in *.md; do
  grep -c x $f
done"
            )
            .activity,
            Activity::Searching
        );
    }

    #[test]
    fn does_not_split_inside_quotes() {
        assert_eq!(
            segments(r#"grep "a|b" x && ls"#),
            vec![r#"grep "a|b" x "#, " ls"]
        );
        assert_eq!(shell(r#"echo "cd; grep x""#).activity, Activity::Other);
    }

    #[test]
    fn recognizes_drift_lookups() {
        let call = shell("drift query foo");
        assert_eq!(call.activity, Activity::Drift);
        assert_eq!(call.finding, Some(Finding::DriftLookup));
        assert_eq!(
            shell("git status && (cd x && drift map)").finding,
            Some(Finding::DriftLookup)
        );
        let indexing = shell("cd x && drift index .");
        assert_eq!(indexing.activity, Activity::Drift);
        assert_eq!(indexing.finding, None);
        assert_eq!(
            shell(r"C:\tools\drift.exe refs foo").finding,
            Some(Finding::DriftLookup)
        );
    }

    #[test]
    fn recognizes_searches_for_one_name() {
        for command in [
            "grep -rn verifyToken src",
            "rg -A 3 verifyToken",
            r#"rg "\bverifyToken\b""#,
            "git grep verifyToken",
        ] {
            assert_eq!(
                shell(command).finding,
                Some(Finding::NameSearch),
                "{command}"
            );
        }
        for command in ["grep -rn 'TODO: fix' src", "rg 'foo.*bar'", "grep -c ab x"] {
            assert_eq!(shell(command).finding, None, "{command}");
        }
    }

    #[test]
    fn only_whole_reads_of_code_count_as_replaceable() {
        let read = |input: Value| classify("Read", &input).finding;
        assert_eq!(
            read(json!({"file_path": "src/a.rs"})),
            Some(Finding::WholeFileRead)
        );
        assert_eq!(read(json!({"file_path": "src/a.rs", "offset": 9})), None);
        assert_eq!(read(json!({"file_path": "notes.txt"})), None);
        assert_eq!(shell("cat src/a.py").finding, Some(Finding::WholeFileRead));
        assert_eq!(shell("cat notes.txt").finding, None);
    }

    #[test]
    fn grep_tool_only_counts_plain_names() {
        let grep = |pattern: &str| classify("Grep", &json!({"pattern": pattern})).finding;
        assert_eq!(grep("verifyToken"), Some(Finding::NameSearch));
        assert_eq!(grep("verify.*Token"), None);
    }

    #[test]
    fn images_add_no_bytes() {
        let content = json!([{"type": "text", "text": "abcd"}, {"type": "image", "source": {"data": "x".repeat(99)}}]);
        assert_eq!(result_bytes(&content), 4);
        assert_eq!(result_bytes(&json!("abcdef")), 6);
    }

    #[test]
    fn repeated_results_count_once() {
        let call = Call {
            activity: Activity::Reading,
            finding: None,
        };
        let item = |id: &str| Seen {
            id: id.to_string(),
            call,
            bytes: 40,
        };
        let report = summarize(vec![item("a"), item("a"), item("b")], 7);
        assert_eq!(report.results.calls, 2);
        assert_eq!(report.results.tokens(), 20);
    }
}
