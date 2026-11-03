# Roadmap

Drift has one job: reduce the tokens an AI agent spends finding and understanding code. Every item here has to serve that job, and has to prove it with a measurement. See [CONTRIBUTING.md](CONTRIBUTING.md) for the rules.

## Where agent tokens go

We measured the text of 31,439 tool results from 104 real Claude Code sessions by one developer. It came to about 38 MB, or roughly 10M tokens at 4 bytes per token.

| Agent activity | Share of tool output |
|----------------|----------------------|
| Reading files: the `Read` tool, plus shell `cat`, `sed` and `head` | about 42% |
| Building, testing and running code | about 11% |
| Searching files: the `Grep` tool, plus shell `grep` and `rg` | about 10% |
| `git` | about 5% |
| Everything else | about 33% |

This is one developer's work, classified by matching commands, so treat these as rough shares. It counts tool output only. It leaves out 4,022 image results, and it doesn't count that earlier output is sent again on each later turn. Phase 0 exists to replace this with a repeatable measurement.

What this tells us:

- **Reading is the biggest cost, by a wide margin.** A whole-file read averaged 7.3 KB, while the median function is about 10 lines. So Phase 2 comes before Phase 3.
- **Search matters less than it looks.** Shell commands containing `grep` were 34% of output, but most of that was `sed`, `cat` or `git` output combined with a `grep`. Searching files directly was about 10%, and searches for a single name were about 3%.
- **Drift already wins when it applies.** One lookup in a real repo cost 77 bytes through Drift, against 10.9 KB for `git grep` and 12.6 KB for reading the file.
- **Drift could also be the problem.** Before output caps, `drift tests guide` printed 103 KB in a large repo, and `drift importers react` printed 21 KB.

## Estimates

Estimates are in Claude Code tokens, input plus output. What widens each range is the number of CI rounds across three operating systems, how far a tree-sitter grammar differs from what we expect, and checking results against real repos.

**Status:** Phases 1 to 4 and 6 are built, along with the benchmark from Phase 0. What's left is `drift audit` and Phase 5, **about 1.2M–2.4M tokens**.

## Phase 0: Measure (`drift audit` remains, about 200K–400K tokens)

Nothing else can be judged without this, so it comes first.

| Item | What it does | Estimate |
|------|--------------|----------|
| **Benchmark suite** (built) | `drift bench` generates the same tasks from any repo, such as "find this definition" or "who imports this file", and compares Drift's output against the `grep` and `Read` route. `bench/run.sh` runs it on pinned public repos, and CI runs that on every PR. | 300K–600K |
| **`drift audit`** (not built) | Reads local Claude Code transcripts and reports token share by tool, and how often agents used Drift or fell back to `grep`. Prints totals only, and never sends anything anywhere. | 200K–400K |

## Phase 1: Keep output small and trustworthy (built)

| Item | What it does | Estimate |
|------|--------------|----------|
| **Output budget** | Every lookup stops at a default cap of about 4 KB and ends with a line such as `… 212 more, use --all or narrow the search`. Adds `--limit`, plus `--in <path>` to search one folder. | 150K–300K |
| **Ranking** | Puts exported source definitions first, then private ones, then tests, fixtures, generated files and old migrations. In one measured repo, 16% of symbols were in migrations or template copies. | 200K–400K |
| **Automatic freshness** | Lookups rebuild the index when it's missing or stale. A full rebuild took 0.02–0.32s on our largest repos, so agents shouldn't have to think about it, and a stale line number wastes a read. | 200K–400K |
| **Fewer retries** | `drift query a b c` answers several names in one call, and a miss suggests close names. | 100K–200K |

## Phase 2: Replace file reads (built)

| Item | What it does | Estimate |
|------|--------------|----------|
| **`drift show <symbol>`** | Prints a definition's source, so a lookup and a read become one call. | 150K–300K |
| **`drift sig <symbol>`** | Prints only the signature and doc comment. Often enough for calling a function, and far smaller than the body. | 150K–300K |
| **`drift tests --for <file>`** | Finds the tests that import a file, using the import graph. | 100K–200K |
| **`drift context <symbol or file>`** | One budgeted answer to "what do I need before changing this?": signature, importers and related tests. | 400K–800K |

## Phase 3: Replace grep (built)

| Item | What it does | Estimate |
|------|--------------|----------|
| **`drift refs <symbol>`** | Finds call sites, grouped by file and enclosing function, with comments and strings left out. Usages are the main reason agents still need `grep`. | 600K–1.2M |
| **`drift find <words>`** | Searches names by word in any order, so `rate limit` finds `enforceRateLimit`. Today's substring match misses it. | 200K–400K |

The real test is whether direct file search falls on real work, which needs `drift audit`.

## Phase 4: Orientation (built)

| Item | What it does | Estimate |
|------|--------------|----------|
| **`drift map`** | A budgeted summary of a repo: main folders, entry points, and the most imported files and symbols. Replaces exploring with `ls` and opening files at the start of a session. | 400K–800K |
| **`drift changes [ref]`** | Lists the symbols changed since a git ref, mapped from diff lines to their enclosing definitions. Lets an agent review a branch without reading whole files. | 300K–600K |

## Phase 5: Coverage (about 1M–2M tokens)

Ordered by what agents actually read, rather than by which languages are popular. Each still has to pass the benchmark.

| Item | Evidence | Estimate |
|------|----------|----------|
| **Markdown** | 10% of `Read` output. An outline of headings with line ranges lets an agent read one section of a long doc. | 150K–300K |
| **GDScript** | 7% of `Read` output, more than Rust and JavaScript combined in this sample. | 200K–400K |
| **Lua** | 2% of `Read` output, in repos with thousands of Lua files. | 200K–400K |
| **PHP** | The largest uncovered language by file count, though much of that may be vendored code. | 200K–400K |
| **JSON and YAML keys** | 4% of `Read` output is JSON. An outline of top-level keys avoids reading large config files. | 150K–300K |
| **Rust package names** | `drift importers` guesses crate names from folder names, so a renamed package gets a wrong "no importers" answer. Read them from `Cargo.toml`. | 100K–200K |

## Phase 6: Adoption (built)

A feature saves nothing if agents don't use it.

| Item | What it does | Estimate |
|------|--------------|----------|
| **`drift install --hook`** | An optional Claude Code hook that points an agent to Drift when it greps for a name the index already knows. | 300K–600K |
| **`drift install --instructions`** | Adds the agent instructions to the right file for a repo, so nobody has to write them by hand. | 100K–200K |
| **Smaller instructions** | Instructions cost tokens in every session. Freshness and ranking will let us shorten them, with a limit enforced in CI. | 50K–100K |

## Decided against

These were considered and rejected, or don't earn their cost yet. New evidence can reopen any of them.

| Idea | Why not |
|------|---------|
| **Embeddings or semantic search** | Adds a model, large dependencies and slow indexing. Try `drift find` first and measure what it misses. |
| **MCP server** | Tool definitions can cost tokens in every session, whether they're used or not. A shell command costs nothing until it runs. |
| **Language server integration** | More accurate, but needs per-language servers and setup, which breaks the single-binary promise. |
| **Incremental indexing** | A full rebuild took at most 0.32s, so there's little to gain. |
| **Smaller index files** | Agents query the index and don't read it, so its size no longer affects tokens. Only what Drift prints does. |
| **More import aliases** | Every `tsconfig` we checked with path aliases uses `@`, which is already handled. |
| **Compressing build and test output** | About 11% of output, but it's a different job from understanding code. Better as a separate tool. |
| **Hosted service, dashboards or telemetry** | Drift stays a local utility. Nothing leaves the machine. |
| **Editing or refactoring code** | Drift reads code and helps find it. Agents already have tools for changing it. |
