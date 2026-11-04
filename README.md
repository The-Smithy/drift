<p align="center">
  <img src="logo.svg" width="180" alt="Drift logo — scattered dots converging to a focal point">
</p>

<h1 align="center">Drift</h1>

<p align="center">
  <strong>Pinpoint any code in your repo.</strong><br>
  A fast, single-binary codebase indexer that gives AI agents a map before they search.
</p>

<p align="center">
  <a href="#install">Install</a> &middot;
  <a href="#quick-start">Quick Start</a> &middot;
  <a href="#commands">Commands</a> &middot;
  <a href="#how-it-works">How It Works</a> &middot;
  <a href="#for-agents">For Agents</a>
</p>

---

## Why

AI coding agents waste tokens on exploration. Finding where `UserController` is defined can cost thousands of tokens in grep output and file reads, just to land on one line.

Drift indexes your code, then answers those questions directly. An agent runs one short lookup and reads only the lines it needs.

```console
$ drift query UserController
class UserController  src/controllers/user.ts:12-140 (exported)
```

### Measured savings

`drift bench` runs a standard set of tasks on any repo and compares Drift's output with what an agent would really print without it. Here it is on nine public projects pinned in [`bench/corpus.txt`](bench/corpus.txt):

| Repo | Language | Tokens without Drift | Tokens with Drift | Saved |
|------|----------|----------------------|-------------------|-------|
| [flask](https://github.com/pallets/flask) | Python | 69,543 | 15,958 | 77.1% |
| [express](https://github.com/expressjs/express) | JavaScript | 37,072 | 5,455 | 85.3% |
| [ripgrep](https://github.com/BurntSushi/ripgrep) | Rust | 69,025 | 16,922 | 75.5% |
| [vue core](https://github.com/vuejs/core) | TypeScript | 45,988 | 14,791 | 67.8% |
| [godot-open-rpg](https://github.com/gdquest-demos/godot-open-rpg) | GDScript | 28,843 | 6,718 | 76.7% |
| [plenary.nvim](https://github.com/nvim-lua/plenary.nvim) | Lua | 39,531 | 7,761 | 80.4% |
| [jq](https://github.com/jqlang/jq) | C | 43,462 | 7,460 | 82.8% |
| [leveldb](https://github.com/google/leveldb) | C++ | 47,354 | 16,590 | 65.0% |
| [serilog](https://github.com/serilog/serilog) | C# | 48,395 | 25,895 | 46.5% |
| **All nine** | | **429,213** | **117,550** | **72.6%** |

And by task, across all nine:

| Task | Without Drift, compared with | Saved |
|------|------------------------------|-------|
| See a signature | Reading about 3.4 KB of the file | 93–98% |
| Find a file's tests | A search of up to about 1.6 KB | 63–95% |
| Understand a file | Reading the file: the whole of it half the time, and otherwise about 3.4 KB | 71–92% |
| Read one function | Reading about 3.4 KB, or the whole function if that's more | 58–90% |
| Orient in the repo | Listing every file, up to about 4.3 KB | 28–97% |
| Find importers | A search of up to about 1.6 KB | 22–93% |
| Find in the docs | A search of up to about 1.6 KB, then reading about 3.4 KB or the whole section | 11–74% |
| Find a definition | A search of up to about 1.6 KB | -2% to 92% |
| Find uses | A search of up to about 1.6 KB | -75% to 60% |

These count tool output at about 4 bytes per token, and use the same costs as `drift savings`. In one developer's recent sessions, 206 name searches printed about 1.6 KB on average, and about half of 522 reads of code files were ranges averaging about 3.4 KB. A search that would print less than 1.6 KB counts as what it would print. Results vary by repo, so run it on yours:

```bash
drift bench .
```

**These numbers used to read 87–97%.** That compared with a search's full output and with reading whole files, and agents rarely print that much. Drift's output didn't change, only what it's compared with. `drift bench` still prints the older total, and `--json` gives it as `uncapped_tokens`, but it's a ceiling and not a saving.

**They're still estimates.** The costs come from one developer's sessions, and the tasks are spread evenly across a repo rather than matching what agents ask most. A negative number means Drift printed more than a trimmed search. That's most common with `drift refs`, which lists uses a trimmed search may have cut off, so the two don't show the same thing.

### Your own savings

Each lookup adds one line to a log on your machine, and `drift savings` adds them up:

```console
$ drift savings
Last 30 days: 56 lookups in 1 repos
Drift printed about 11.6K tokens. The usual searches and reads would have printed about 55.0K.
Estimated saving: about 43.5K tokens (79.0%)

By command:
  outline      18 uses     2.6K printed    35.5K instead    32.9K saved
  show          9 uses     4.6K printed     7.7K instead     3.0K saved
  tests         8 uses     1.4K printed     4.2K instead     2.8K saved
  query        11 uses      245 printed     2.9K instead     2.6K saved
  ...
```

Each lookup is compared with what it most likely replaced, at the average cost of real ones:

| Lookup | Compared with |
|--------|---------------|
| `query`, `find`, `docs`, `refs`, `importers`, `tests` | One search of about 1.6 KB, per name for `query` |
| `outline` | Reading the file: the whole of it half the time, and otherwise about 3.4 KB |
| `show`, `sig` | Reading about 3.4 KB, or what was printed if that's more |
| `context` | One such read and two searches |
| `map`, and `docs` or `tests` with no words | One file listing of about 4.3 KB |

- **It's an estimate.** It can't know what an agent would really have done, or whether it would have needed several searches.
- **It leans low.** A lookup that finds nothing counts as a cost, and so do the hook's notes. Only `outline` counts a whole file, so `outline` then `show` isn't counted as two full reads. A lookup can count as a loss too: a long `refs` often prints more than a trimmed grep.
- **It leaves out `drift changes`,** which has no fair comparison, and the tokens the skill and instructions cost.
- **It's private.** The log holds the time, the repo's name, the command, whether it found anything and two byte counts. It has no search terms, paths or code, and nothing leaves your machine.

Set `DRIFT_NO_SAVINGS=1` to turn logging off. It's also off when `CI` is set. The log stays under about 8 MB.

`drift audit` is the other half: it shows the searches and reads that could have been Drift lookups, which `drift savings` can't see.

**Tested across 27 real repos:** 4,595 files and 924,000 lines, giving 43,604 symbols and 5,101 test cases, indexed in about 2.5 seconds on one Windows machine. Every symbol was checked against its source file, and the name was at the recorded location in all of them.

## Install

### Prebuilt binary

Download the archive for your platform from the [latest release](https://github.com/The-Smithy/drift/releases/latest). It holds a single `drift` binary with no runtime dependencies, so you don't need Rust or a C compiler.

| Platform | Archive |
|----------|---------|
| Windows x86_64 | [`drift-x86_64-pc-windows-msvc.zip`](https://github.com/The-Smithy/drift/releases/latest/download/drift-x86_64-pc-windows-msvc.zip) |
| Linux x86_64 | [`drift-x86_64-unknown-linux-musl.tar.gz`](https://github.com/The-Smithy/drift/releases/latest/download/drift-x86_64-unknown-linux-musl.tar.gz) |
| macOS Apple silicon | [`drift-aarch64-apple-darwin.tar.gz`](https://github.com/The-Smithy/drift/releases/latest/download/drift-aarch64-apple-darwin.tar.gz) |
| macOS Intel | [`drift-x86_64-apple-darwin.tar.gz`](https://github.com/The-Smithy/drift/releases/latest/download/drift-x86_64-apple-darwin.tar.gz) |

On Linux or macOS, this installs it to `~/.local/bin`, using the archive name for your platform:

```bash
mkdir -p ~/.local/bin
curl -fsSL https://github.com/The-Smithy/drift/releases/latest/download/drift-x86_64-unknown-linux-musl.tar.gz | tar xz -C ~/.local/bin drift
```

On Windows, in PowerShell:

```powershell
Invoke-WebRequest https://github.com/The-Smithy/drift/releases/latest/download/drift-x86_64-pc-windows-msvc.zip -OutFile drift.zip
Expand-Archive drift.zip "$env:LOCALAPPDATA\Programs\drift"
```

Then add that folder to your `PATH`.

The binaries aren't signed. If macOS blocks one you downloaded in a browser, run `xattr -d com.apple.quarantine drift`. Each release also has a `SHA256SUMS` file for checking your download.

### From source

Installing from source needs a recent stable [Rust](https://rustup.rs) and a C compiler, because the tree-sitter grammars are written in C.

```bash
cargo install --git https://github.com/The-Smithy/drift
```

This puts a single `drift` binary on your `PATH`. It has no runtime dependencies.

## Quick Start

There's nothing to set up. Run a lookup anywhere in a git repo, and Drift builds its index on first use and keeps it fresh after that.

```bash
drift map                         # Get oriented in a repo
drift query UserController        # Find a definition
drift show UserController         # Read just that definition
drift outline src/lib/auth.ts     # See what a file contains
drift refs verifyToken            # Find where something is used
drift context src/lib/auth.ts     # Get what you need before changing it
```

**Teach your agent to use it:**

```bash
drift install . --instructions
```

This adds a skill that agents load when they need it, a short section in `AGENTS.md`, and a `.gitignore` entry for the index. It works with Claude Code, Codex, Cursor, Copilot and [many other tools](#for-agents).

## Commands

| Command | Description |
|---------|-------------|
| `drift query <symbol>...` | Find where symbols are defined. Exact names match first, then case-insensitive substrings |
| `drift find <words>...` | Search names, doc comments and paths by words in any order, so `rate limit` finds `enforceRateLimit` |
| `drift docs [words]...` | Find the sections of your documents that cover something, ranked, as pointers rather than text. With no words, list the documents |
| `drift show <symbol>` | Print one definition's source. Takes a name, `Parent.name`, `Parent::name`, `path:name`, `path:line` or a range of lines, `path:start-end` |
| `drift sig <symbol>...` | Print only a definition's signature and doc comment |
| `drift outline <file>` | List a file's definitions with line ranges, and its imports. Add `--sig` for signatures |
| `drift refs <symbol>` | Find uses, by file and enclosing definition, leaving out comments, strings and imports. Matches by name, not by type |
| `drift tests [text]` | With no text, summarize every test file. With text, show the tests whose path, suite or name contains it |
| `drift tests --for <file>` | Find the tests for a source file |
| `drift importers <module>` | Find the files that import a module. A file path resolves relative imports and `@/` or `~/` aliases in JS and TS, module paths in Python and Lua, `use` paths and `mod` declarations in Rust, `#include` paths in C and C++, and `res://` paths in GDScript. Anything else matches as text in the import source, which is how to search C# namespaces |
| `drift context <target>` | Get what you need before changing a symbol or file: signature, uses, importers and tests |
| `drift map` | Summarize a repo: folders, entry points and most imported files |
| `drift changes [ref]` | List the definitions changed since a git ref, including uncommitted work |
| `drift bench [path]` | Measure token savings on a repo with a standard set of tasks |
| `drift savings` | Estimate the tokens Drift has saved you, from a log kept on your machine. See [Your own savings](#your-own-savings) |
| `drift audit` | Report where agent tokens went, from your local Claude Code transcripts, and how often Drift was used. Totals only |
| `drift index [path]` | Build the index yourself. Lookups normally do this on their own |
| `drift stats` | Show index statistics |
| `drift batch <parent>` | Index every git repo directly under a directory |
| `drift install [path]` | Set up a repo so coding agents use Drift. See [For Agents](#for-agents) |
| `drift update-check` | Check for a newer release now and show how to install it |

### Options

| Flag | Command | Description |
|------|---------|-------------|
| `--compact` | `index`, `batch` | Strip default values, minify, and limit `symbol_index` to exported symbols (about 67% smaller) |
| `-o, --output` | `index` | Custom output path (default: `.drift-index.json` in the repo root) |
| `--ignore-dirs` | `index` | Extra directory names to skip |
| `--ignore-files` | `index` | Extra file names to skip |
| `--install` | `batch` | Also install the Claude Code skill in each repo |
| `--instructions` | `install` | Also add a section to `AGENTS.md`, and to `CLAUDE.md` and `GEMINI.md` if the repo has them. `--file` picks one file instead |
| `--agent <tool>` | `install` | Also set up `kiro`, `junie`, `continue`, `aider` or `all`, before the repo uses them |
| `--hook` | `install` | Also add a Claude Code hook that points agents to Drift. It never blocks a tool |
| `--in <path>` | Lookups | Only look in this folder |
| `--limit <n>` | Lookups | Show at most this many lines, replacing the default size cap |
| `--all` | Lookups | Show everything |
| `--index <file>` | Lookups, `stats` | Use this index file as is, without refreshing it |
| `--json` | `bench`, `audit`, `savings` | Print the report as JSON. `bench` can compare a saved report with `--compare` |
| `--compare <file>` | `bench` | Compare with a report saved earlier |
| `--check <percent>` | `bench` | Fail unless savings reach this percent and 90% of answers are right |
| `--days <n>` | `audit` | How many days back to look (default: 30) |
| `--project <text>` | `audit` | Only count projects whose transcript folder contains this text |
| `--dir <folder>` | `audit` | Read transcripts from this folder instead of `~/.claude/projects` |

Lookups stop at a size cap, 4 KB for most and 12 KB for `show`, and say how much they left out. Results come best first: exported source, private source, tests, then fixtures, generated files, documents and migrations.

## Supported Languages

| Language | Extensions | Symbols extracted |
|----------|-----------|-------------------|
| **TypeScript** | `.ts`, `.tsx`, `.mts`, `.cts` | functions, arrow functions, classes, methods, interfaces, types, enums, consts, namespaces |
| **JavaScript** | `.js`, `.jsx`, `.mjs`, `.cjs` | functions, arrow functions, classes, methods, consts, CommonJS exports |
| **Python** | `.py` | functions, methods, classes, constants, type aliases, decorators |
| **Rust** | `.rs` | functions, methods, structs, enums, traits, impls, type aliases, consts, statics, macros, modules |
| **SQL** | `.sql` | tables, views, functions, procedures, triggers, indexes, types, policies |
| **C** | `.c`, `.h` | functions, prototypes, structs, unions, enums, typedefs, macros, globals |
| **C++** | `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hh`, `.hxx`, `.ipp`, `.inl`, and `.h` files that contain C++ | the above plus classes, methods, templates, namespaces, type aliases. Export macros and Unreal's `UCLASS`-style macros are handled |
| **C#** | `.cs` | classes, records, structs, interfaces, enums, delegates, namespaces, methods, properties, constants |
| **GDScript** | `.gd` | classes, functions, signals, enums, constants, variables |
| **Lua** | `.lua` | functions, table functions and methods, constants, tables |
| **Markdown** | `.md`, `.mdx`, `.markdown` | headings, each with the line range of its section |
| **reStructuredText** | `.rst` | headings, each with the line range of its section |
| **AsciiDoc** | `.adoc`, `.asciidoc` | headings, each with the line range of its section |
| **Plain text** | `.txt` in the repo root or in folders such as `docs`, `notes` and `specs` | sections split at paragraph breaks. Lists and other files that aren't sentences are left out of searches |
| **JSON** | `.json`, `.jsonc` | keys, two levels deep |
| **YAML** | `.yaml`, `.yml` | keys, two levels deep |

### Test Discovery

| Language | Finds | Frameworks reported |
|----------|-------|---------------------|
| TypeScript/JS | `describe`, `it`, `test` and their `.only`, `.skip`, `.each` forms, nested suites | Vitest, Jest, Mocha, Playwright, `node:test`, Cypress |
| Python | `test_*` functions, `Test*` classes, and `TestCase` subclasses, including through a base class in the same file | pytest, unittest |
| Rust | `#[test]`, `#[tokio::test]`, `#[rstest]`, and tests inside macros such as `proptest!` | cargo-test |
| C and C++ | `TEST`, `TEST_F`, `TEST_CASE`, `BOOST_AUTO_TEST_CASE` and similar, in files that include the framework | GoogleTest, Catch2, doctest, Boost.Test, Criterion, Check, Unity, cmocka |
| C# | Methods with `[Fact]`, `[Theory]`, `[Test]`, `[TestCase]` or `[TestMethod]` | xUnit, NUnit, MSTest |
| GDScript | `test_*` functions in scripts or inner classes that extend the framework's class | GUT, gdUnit4 |
| Lua | `describe` and `it`, or `test*` functions in files that use LuaUnit | busted, LuaUnit |

A JS or TS test file that names no framework takes one from the nearest `package.json`. If that doesn't settle it, the framework is reported as `unknown`.

## How It Works

Drift walks your repo in parallel and parses each file with [tree-sitter](https://tree-sitter.github.io/). Parsing rather than pattern matching means commented-out code and code inside strings are not mistaken for definitions. SQL is the exception: dialects vary too much for one grammar, so `CREATE` statements are matched by pattern, with comments removed first. Documents are also read without a grammar, skipping Markdown's code blocks, comments and front matter. JSON and YAML keys, like headings, rank below code.

Document text isn't stored in the index, which every lookup has to load. `drift docs` reads it when you search, and ranks sections with [BM25](https://en.wikipedia.org/wiki/Okapi_BM25), counting words in headings and file names for more. It took about 60 ms a search on 442 documents.

For each file it records:

1. **Symbols:** definitions with their line ranges, signatures, first doc comment line, whether they are exported, and their parent class, impl or module
2. **Imports:** what the file depends on, including re-exports, `require()` and dynamic `import()`
3. **Tests:** suites and individual cases with line numbers

### What it doesn't capture

- Types. `drift refs` finds uses by name when you ask, so unrelated things that share a common name can appear.
- Generated symbols, such as Rust macro expansions or names built at runtime.
- Local variables and functions nested inside JS or TS functions. Python nested functions are included.
- Interface and struct members, apart from methods, and C# properties and constants.
- C and C++ code that only exists after the preprocessor runs. Export macros and Unreal's macros are handled, but unusual macros can hide a definition. In testing, jq lost 7 of 662 functions this way.
- Which C# file a `using` refers to, because it names a namespace. `drift importers App.Core` searches them as text, and `drift tests --for` also matches test files named after the source file, such as `ServiceTests.cs`.
- JSON and YAML keys past two levels, or past 300 in one file.
- Meanings in documents. `drift docs` matches words and their forms, so `expires` won't find a section that only says `lifetime`. It says which words each result lacks, and an agent can try others.
- Documentation inside dependencies, since `node_modules` and similar folders are skipped.
- Non-test items inside Rust `#[cfg(test)]` modules, which are left out to keep the index small.
- Declarations caught up in syntax the grammar can't read. About 0.5% of files had some in testing. Those files are still indexed, and declarations elsewhere in them are still found.

SQL `CREATE` statements inside dollar-quoted function bodies are counted as definitions, because the pattern matcher doesn't parse those bodies.

### Ignored by default

Drift respects `.gitignore` and `.ignore` files and skips hidden directories, `node_modules`, `dist`, `build`, `target`, `vendor`, `coverage`, `venv` and similar directories. It also skips lockfiles, `*.min.js`, Drift's own index and files over 512 KB.

### Update notices

Once a day at most, a lookup starts a short background check for a newer release, so it never waits on the network. The check is an ordinary `git ls-remote` of this repo, and nothing about you or your code is sent. If there's a newer release, Drift prints one line to stderr, at most once a day, with the command to update. It never updates itself.

Set `DRIFT_NO_UPDATE_CHECK=1` to turn this off. It's also off when `CI` is set.

## Index Schema

```json
{
  "version": "1.1",
  "repo": "my-project",
  "root": "/home/me/my-project",
  "generated_at": "2026-11-03T14:00:00Z",
  "languages": ["python", "typescript"],
  "files": {
    "src/auth.ts": {
      "lang": "typescript",
      "lines": 142,
      "symbols": [
        {"name": "AuthService", "kind": "class", "line": 12, "end_line": 88, "exported": true},
        {"name": "validateToken", "kind": "function", "line": 45, "end_line": 60, "exported": true, "async": true, "parent": "AuthService"}
      ],
      "imports": [
        {"source": "./config", "names": ["JWT_SECRET"]},
        {"source": "jsonwebtoken", "names": ["verify", "sign"]}
      ]
    }
  },
  "tests": {
    "tests/auth.test.ts": {
      "framework": "vitest",
      "suites": [
        {"name": "AuthService", "line": 5, "cases": [
          {"name": "validates a good token", "line": 8},
          {"name": "rejects an expired token", "line": 15}
        ]}
      ]
    }
  },
  "symbol_index": {
    "AuthService": [{"file": "src/auth.ts", "line": 12, "end_line": 88, "kind": "class"}],
    "validateToken": [{"file": "src/auth.ts", "line": 45, "end_line": 60, "kind": "function"}]
  }
}
```

This example shows compact output. Without `--compact`, every field is present, including nulls and `false` values, and `symbol_index` also lists private symbols.

## For Agents

See [AGENTS.md](AGENTS.md) for detailed integration instructions.

**The short version:** use Drift's lookups before grepping, then read only the lines you need. Don't read the index file itself. A large repo's index runs to hundreds of kilobytes, and with `--compact` it is a single line.

### Setting up a repo

```bash
drift install . --instructions
```

This writes files that nearly every coding agent already reads:

| Tool | Skill, loaded when needed | Instructions, loaded every session |
|------|---------------------------|------------------------------------|
| Claude Code | `.claude/skills/` | `CLAUDE.md`, if the repo has one |
| Codex, Cursor, Copilot, Windsurf, Amp, Antigravity, OpenCode, Factory Droid | `.agents/skills/` | `AGENTS.md` |
| Zed | `.agents/skills/` | `AGENTS.md`, unless a `.rules`, `.cursorrules` or `CLAUDE.md` file takes priority |
| Cline | `.claude/skills/` | `AGENTS.md` |
| Gemini CLI | `.agents/skills/` | `GEMINI.md`, if the repo has one |
| Kiro | `.kiro/skills/` | `AGENTS.md` |
| JetBrains Junie | `.junie/skills/` | `AGENTS.md` |
| Continue | None | `.continue/rules/drift.md` |
| Aider | None | `CONVENTIONS.md`, added to `read:` in `.aider.conf.yml` |

The last four tools get their own files only when the repo already uses them, so a repo doesn't collect files for tools nobody runs. To set one up anyway, add `--agent kiro`, or `--agent all` for all four.

This table comes from each tool's documentation. We haven't run Drift inside every one of these tools, so please open an issue if one doesn't pick it up.

If you only want the skill and not the always-loaded section, leave out `--instructions`. To put the section in a different file, such as one that another tool regenerates around, use `--instructions --file SITE.md`.

Instructions are easy for an agent to skip once it's busy, so Claude Code users can also add `--hook`. Just before a search or a large read, it adds a one-line note naming the Drift command that answers it, such as where a name is defined. Each note appears once a session, and it never blocks a tool. See [AGENTS.md](AGENTS.md#claude-code) for what it catches. Hooks are specific to Claude Code.

### Other AI tools

Add this to your agent's instructions file:

> Before grepping or reading whole files, use Drift: `drift query <symbol>` finds a definition, `drift show <symbol>` prints it, `drift outline <file>` lists a file's definitions and imports, `drift refs <symbol>` finds uses, and `drift map` orients you in a repo. Do not read `.drift-index.json` directly, because it is large.

## Contributing

Drift's only goal is to reduce the tokens agents spend on code. See [ROADMAP.md](ROADMAP.md) for what's planned and what was ruled out, and [CONTRIBUTING.md](CONTRIBUTING.md) for how to contribute.

## License

MIT
