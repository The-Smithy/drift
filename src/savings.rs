//! Estimates what Drift saves as it's used. Each lookup adds one line to a log on this machine:
//! the command, the repo's name, the bytes printed and what the usual search or read would have
//! printed. It holds no search terms or file names, and nothing leaves the machine.
//!
//! What a search or read would have cost comes from real sessions, not from what a tool could
//! print at most. Agents trim their searches with `head`, `-l` and filters, and about half their
//! reads of code files are ranges, so an estimate from whole files and full greps runs far too high.
//! `drift bench` uses the same costs, so the two agree.
//!
//! Set `DRIFT_NO_SAVINGS` to turn this off. It's also off when `CI` is set.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::audit::short;
use crate::bench::BYTES_PER_TOKEN;
use crate::model::Index;
use crate::update::cache_dir;

const LOG_FILE: &str = "savings.jsonl";
const OLD_LOG_FILE: &str = "savings.old.jsonl";
const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;

// The average name search across 206 real ones, most of them already trimmed by the agent.
pub(crate) const SEARCH_BYTES: u64 = 1650;
// The average ranged read of a code file across 268 real ones.
pub(crate) const RANGED_READ_BYTES: u64 = 3400;
// About half of 522 real reads of code files took the whole file.
const WHOLE_READ_PERCENT: u64 = 50;
// The average listing across 34 real ones.
pub(crate) const LISTING_BYTES: u64 = 4300;
// Agent tools stop a read here, so a longer file doesn't cost more.
pub(crate) const MAX_READ_LINES: u64 = 2000;

/// What an agent would most likely have done without this lookup.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Alternative {
    /// This many searches.
    Searches(u64),
    /// Reading a whole file or a range of it, to see what the file the answer names contains.
    ReadFile,
    /// Reading a range of the file the answer names first.
    ReadRange,
    /// Reading a range of each file the answer names.
    ReadRanges,
    /// Reading a range of the file the answer names first, and this many searches.
    RangeAndSearch(u64),
    /// Listing files.
    Listing,
    /// Nothing that compares fairly, so it's left out of the totals.
    Unmeasured,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
struct Entry {
    /// Seconds since 1970.
    t: u64,
    repo: String,
    cmd: String,
    /// Whether the lookup found what it was asked for.
    ok: bool,
    /// Bytes printed.
    out: u64,
    /// Bytes the alternative would have printed, if there's a fair comparison.
    alt: Option<u64>,
}

fn log_dir() -> Option<PathBuf> {
    let off = std::env::var_os("DRIFT_NO_SAVINGS").is_some() || std::env::var_os("CI").is_some();
    if off { None } else { cache_dir() }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

// Must never get in the way of the answer, so any failure here just means one line less.
fn append(entry: &Entry) {
    let Some(dir) = log_dir() else { return };
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join(LOG_FILE);
    if fs::metadata(&path).is_ok_and(|meta| meta.len() > MAX_LOG_BYTES) {
        let _ = fs::rename(&path, dir.join(OLD_LOG_FILE));
    }
    let Ok(line) = serde_json::to_string(entry) else {
        return;
    };
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = file.write_all(format!("{line}\n").as_bytes());
    }
}

/// The indexed files an answer names, in order. A path can end in `:line` or `:start-end`.
fn named_files<'a>(index: &'a Index, text: &str) -> Vec<&'a str> {
    let mut seen = HashSet::new();
    let mut found = Vec::new();
    for word in text.split_whitespace() {
        let path = word.split(':').next().unwrap_or(word);
        if let Some((key, _)) = index.files.get_key_value(path)
            && seen.insert(key.as_str())
        {
            found.push(key.as_str());
        }
    }
    found
}

// Agent tools stop a read at MAX_READ_LINES, so a longer file doesn't cost more.
fn readable_bytes(file_bytes: u64, lines: u64) -> u64 {
    if lines > MAX_READ_LINES {
        file_bytes * MAX_READ_LINES / lines
    } else {
        file_bytes
    }
}

/// What reading the part of a file an agent needs would print: at least what it needed, and
/// no more than the file. Agents read about this much whether or not they read more elsewhere.
pub(crate) fn range_cost(file_bytes: u64, lines: u64, needed: u64) -> u64 {
    readable_bytes(file_bytes, lines).min(needed.max(RANGED_READ_BYTES))
}

/// What reading to learn what a file contains would print: the whole file about half the time,
/// and otherwise a range.
pub(crate) fn file_cost(file_bytes: u64, lines: u64) -> u64 {
    let whole = readable_bytes(file_bytes, lines);
    let ranged = range_cost(file_bytes, lines, 0);
    (whole * WHOLE_READ_PERCENT + ranged * (100 - WHOLE_READ_PERCENT)) / 100
}

fn size_of(index: &Index, path: &str) -> (u64, u64) {
    let bytes = fs::metadata(Path::new(&index.root).join(path)).map_or(0, |meta| meta.len());
    let lines = index
        .files
        .get(path)
        .map_or(0, |file| u64::from(file.lines));
    (bytes, lines)
}

// `outline` then `show` on one file would have been one read, so only `outline` counts the whole
// file. Counting it for both would claim a saving nobody got.
fn alternative_bytes(index: &Index, alternative: Alternative, text: &str) -> Option<u64> {
    let out = text.len() as u64;
    let files = named_files(index, text);
    let first_range = |needed| {
        files.first().map_or(0, |path| {
            let (bytes, lines) = size_of(index, path);
            range_cost(bytes, lines, needed)
        })
    };
    Some(match alternative {
        Alternative::Unmeasured => return None,
        Alternative::Searches(count) => count * SEARCH_BYTES,
        Alternative::Listing => LISTING_BYTES,
        Alternative::ReadFile => files.first().map_or(0, |path| {
            let (bytes, lines) = size_of(index, path);
            file_cost(bytes, lines)
        }),
        Alternative::ReadRange => first_range(out),
        Alternative::ReadRanges => {
            let each = out / files.len().max(1) as u64;
            files
                .iter()
                .map(|path| {
                    let (bytes, lines) = size_of(index, path);
                    range_cost(bytes, lines, each)
                })
                .sum()
        }
        Alternative::RangeAndSearch(count) => first_range(0) + count * SEARCH_BYTES,
    })
}

/// Logs one lookup. One that found nothing saved nothing, so what it printed counts as a cost.
pub fn record(index: &Index, command: &str, alternative: Alternative, text: &str, found: bool) {
    if log_dir().is_none() {
        return;
    }
    let alt = if found {
        alternative_bytes(index, alternative, text)
    } else {
        Some(0)
    };
    append(&Entry {
        t: now(),
        repo: index.repo.clone(),
        cmd: command.to_string(),
        ok: found,
        out: text.len() as u64,
        alt,
    });
}

/// Logs a note from the hook, which costs what it prints and replaces nothing by itself.
pub fn record_note(repo: &str, note: &str) {
    append(&Entry {
        t: now(),
        repo: repo.to_string(),
        cmd: "hook note".to_string(),
        ok: true,
        out: note.len() as u64,
        alt: Some(0),
    });
}

#[derive(Serialize, Default, Debug, Clone, PartialEq)]
struct Totals {
    lookups: u64,
    printed_tokens: u64,
    instead_tokens: u64,
    saved_tokens: i64,
    saved_percent: f64,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
struct Row {
    name: String,
    #[serde(flatten)]
    totals: Totals,
}

#[derive(Serialize, Debug, PartialEq)]
struct Report {
    days: u64,
    bytes_per_token: u64,
    total: Totals,
    by_command: Vec<Row>,
    by_repo: Vec<Row>,
    failed_lookups: u64,
    not_measured: u64,
}

#[derive(Default, Clone, Copy)]
struct Sum {
    lookups: u64,
    printed: u64,
    instead: u64,
}

impl Sum {
    fn add(&mut self, out: u64, alt: u64) {
        self.lookups += 1;
        self.printed += out;
        self.instead += alt;
    }

    fn totals(self) -> Totals {
        let saved = self.instead as i64 - self.printed as i64;
        Totals {
            lookups: self.lookups,
            printed_tokens: self.printed / BYTES_PER_TOKEN,
            instead_tokens: self.instead / BYTES_PER_TOKEN,
            saved_tokens: saved / BYTES_PER_TOKEN as i64,
            saved_percent: if self.instead == 0 {
                0.0
            } else {
                (saved as f64 * 1000.0 / self.instead as f64).round() / 10.0
            },
        }
    }
}

fn rows(sums: BTreeMap<String, Sum>) -> Vec<Row> {
    let mut rows: Vec<Row> = sums
        .into_iter()
        .map(|(name, sum)| Row {
            name,
            totals: sum.totals(),
        })
        .collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.totals.saved_tokens));
    rows
}

fn build_report(entries: &[Entry], days: u64, since: u64) -> Report {
    let mut total = Sum::default();
    let mut by_command: BTreeMap<String, Sum> = BTreeMap::new();
    let mut by_repo: BTreeMap<String, Sum> = BTreeMap::new();
    let mut failed_lookups = 0;
    let mut not_measured = 0;
    for entry in entries.iter().filter(|entry| entry.t >= since) {
        let Some(alt) = entry.alt else {
            not_measured += 1;
            continue;
        };
        if !entry.ok {
            failed_lookups += 1;
        }
        total.add(entry.out, alt);
        by_command
            .entry(entry.cmd.clone())
            .or_default()
            .add(entry.out, alt);
        by_repo
            .entry(entry.repo.clone())
            .or_default()
            .add(entry.out, alt);
    }
    Report {
        days,
        bytes_per_token: BYTES_PER_TOKEN,
        total: total.totals(),
        by_command: rows(by_command),
        by_repo: rows(by_repo),
        failed_lookups,
        not_measured,
    }
}

fn read_entries(dir: &Path) -> Vec<Entry> {
    [OLD_LOG_FILE, LOG_FILE]
        .iter()
        .filter_map(|name| fs::read_to_string(dir.join(name)).ok())
        .flat_map(|text| {
            text.lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect::<Vec<Entry>>()
        })
        .collect()
}

fn signed(tokens: i64) -> String {
    if tokens < 0 {
        format!("-{}", short(tokens.unsigned_abs()))
    } else {
        short(tokens as u64)
    }
}

fn print_rows(title: &str, rows: &[Row]) {
    println!("{title}:");
    let width = rows.iter().map(|row| row.name.len()).max().unwrap_or(0);
    for row in rows {
        let totals = &row.totals;
        println!(
            "  {:width$}  {:>6} uses  {:>7} printed  {:>7} instead  {:>7} saved",
            row.name,
            totals.lookups,
            short(totals.printed_tokens),
            short(totals.instead_tokens),
            signed(totals.saved_tokens),
        );
    }
}

fn print_report(report: &Report) {
    let total = &report.total;
    println!(
        "Last {} days: {} lookups in {} repos",
        report.days,
        total.lookups,
        report.by_repo.len()
    );
    println!(
        "Drift printed about {} tokens. The usual searches and reads would have printed about {}.",
        short(total.printed_tokens),
        short(total.instead_tokens)
    );
    println!(
        "Estimated saving: about {} tokens ({:.1}%)",
        signed(total.saved_tokens),
        total.saved_percent
    );
    println!();
    print_rows("By command", &report.by_command);
    println!();
    print_rows("By repo", &report.by_repo);
    println!();
    if report.failed_lookups > 0 {
        println!(
            "{} lookups found nothing. What they printed counts as a cost.",
            report.failed_lookups
        );
    }
    if report.not_measured > 0 {
        println!(
            "{} `drift changes` runs left out, since nothing compares fairly.",
            report.not_measured
        );
    }
    println!(
        "This is an estimate. \"Instead\" uses the average cost of real searches and reads, and can't know what an agent would really have done."
    );
}

pub fn run(days: u64, json: bool) -> Result<()> {
    let entries = cache_dir()
        .map(|dir| read_entries(&dir))
        .unwrap_or_default();
    let since = now().saturating_sub(days.saturating_mul(24 * 60 * 60));
    let report = build_report(&entries, days, since);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if report.total.lookups == 0 && report.not_measured == 0 {
        println!(
            "No lookups recorded in the last {days} days. Drift records them on this machine as they run."
        );
    } else {
        print_report(&report);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(repo: &str, cmd: &str, ok: bool, out: u64, alt: Option<u64>) -> Entry {
        Entry {
            t: 100,
            repo: repo.to_string(),
            cmd: cmd.to_string(),
            ok,
            out,
            alt,
        }
    }

    #[test]
    fn a_range_is_what_was_needed_but_no_more_than_the_file() {
        assert_eq!(range_cost(20_000, 500, 100), RANGED_READ_BYTES);
        assert_eq!(range_cost(20_000, 500, 8_000), 8_000);
        assert_eq!(range_cost(1_000, 20, 100), 1_000);
    }

    #[test]
    fn learning_a_file_costs_all_of_it_half_the_time_and_a_range_otherwise() {
        assert_eq!(file_cost(20_000, 500), (20_000 + RANGED_READ_BYTES) / 2);
        assert_eq!(file_cost(1_000, 20), 1_000);
    }

    #[test]
    fn a_long_file_only_costs_what_a_read_tool_would_print() {
        assert_eq!(
            file_cost(100_000, 10_000),
            file_cost(20_000, MAX_READ_LINES)
        );
    }

    #[test]
    fn totals_add_up_and_can_be_negative() {
        let entries = [
            entry("a", "query", true, 400, Some(4000)),
            entry("a", "refs", true, 4000, Some(1600)),
            entry("b", "query", false, 80, Some(0)),
            entry("b", "changes", true, 500, None),
        ];
        let report = build_report(&entries, 30, 0);
        assert_eq!(report.total.lookups, 3);
        assert_eq!(report.total.printed_tokens, 1120);
        assert_eq!(report.total.instead_tokens, 1400);
        assert_eq!(report.total.saved_tokens, 280);
        assert_eq!(report.total.saved_percent, 20.0);
        assert_eq!(report.failed_lookups, 1);
        assert_eq!(report.not_measured, 1);
        let refs = report.by_command.iter().find(|row| row.name == "refs");
        assert_eq!(refs.unwrap().totals.saved_tokens, -600);
        assert_eq!(report.by_repo[0].name, "a");
    }

    #[test]
    fn old_entries_are_left_out() {
        let entries = [entry("a", "query", true, 400, Some(4000))];
        assert_eq!(build_report(&entries, 30, 101).total.lookups, 0);
        assert_eq!(build_report(&entries, 30, 100).total.lookups, 1);
    }

    #[test]
    fn nothing_saved_shows_as_zero_percent() {
        let entries = [entry("a", "query", false, 80, Some(0))];
        let report = build_report(&entries, 30, 0);
        assert_eq!(report.total.saved_percent, 0.0);
        assert_eq!(report.total.saved_tokens, -20);
        assert_eq!(signed(-20), "-20");
    }
}
