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

`drift bench` runs a standard set of tasks on any repo. Here it is on four public projects pinned in [`bench/corpus.txt`](bench/corpus.txt):

| Repo | Tokens without Drift | Tokens with Drift | Saved |
|------|----------------------|-------------------|-------|
| [flask](https://github.com/pallets/flask) | 197,213 | 9,886 | 95.0% |
| [express](https://github.com/expressjs/express) | 48,142 | 2,953 | 93.9% |
| [ripgrep](https://github.com/BurntSushi/ripgrep) | 301,618 | 11,692 | 96.1% |
| [vue core](https://github.com/vuejs/core) | 155,110 | 10,771 | 93.1% |

And by task, across all four:

| Task | Without Drift, compared with | Saved |
|------|------------------------------|-------|
| See a signature | Reading the file | 98–99% |
| Read one function | Reading the file | 91–99% |
| Find a definition | Grep | 89–97% |
| Understand a file | Reading the file | 87–95% |
| Find a file's tests | Grep | 80–99% |
| Find uses | Grep | 56–81% |
| Find importers | Grep | 68–97% |
| Orient in the repo | Listing every file | 52–92% |

These count tool output at about 4 bytes per token. The comparison is generous to the alternatives: searches stop at 250 lines and reads at 2,000, as they do in Claude Code. Results vary by repo, so run it on yours:

```bash
drift bench .
```

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

This adds the Claude Code skill, a short section in `CLAUDE.md` or `AGENTS.md`, and a `.gitignore` entry for the index.

## Commands

| Command | Description |
|---------|-------------|
| `drift query <symbol>...` | Find where symbols are defined. Exact names match first, then case-insensitive substrings |
| `drift find <words>...` | Search names, doc comments and paths by words in any order, so `rate limit` finds `enforceRateLimit` |
| `drift show <symbol>` | Print one definition's source. Takes a name, `Parent.name` or `path:name` |
| `drift sig <symbol>...` | Print only a definition's signature and doc comment |
| `drift outline <file>` | List a file's definitions with line ranges, and its imports. Add `--sig` for signatures |
| `drift refs <symbol>` | Find uses, by file and enclosing definition, leaving out comments, strings and imports. Matches by name, not by type |
| `drift tests [text]` | With no text, summarize every test file. With text, show the tests whose path, suite or name contains it |
| `drift tests --for <file>` | Find the tests for a source file |
| `drift importers <module>` | Find the files that import a module. A file path resolves relative imports and `@/` or `~/` aliases in JS and TS, module paths in Python, and `use` paths and `mod` declarations in Rust. Anything else matches as text in the import source |
| `drift context <target>` | Get what you need before changing a symbol or file: signature, uses, importers and tests |
| `drift map` | Summarize a repo: folders, entry points and most imported files |
| `drift changes [ref]` | List the definitions changed since a git ref, including uncommitted work |
| `drift bench [path]` | Measure token savings on a repo with a standard set of tasks |
| `drift index [path]` | Build the index yourself. Lookups normally do this on their own |
| `drift stats` | Show index statistics |
| `drift batch <parent>` | Index every git repo directly under a directory |
| `drift install [path]` | Install the Claude Code skill and ignore the index in a repo |

### Options

| Flag | Command | Description |
|------|---------|-------------|
| `--compact` | `index`, `batch` | Strip default values, minify, and limit `symbol_index` to exported symbols (about 67% smaller) |
| `-o, --output` | `index` | Custom output path (default: `.drift-index.json` in the repo root) |
| `--ignore-dirs` | `index` | Extra directory names to skip |
| `--ignore-files` | `index` | Extra file names to skip |
| `--install` | `batch` | Also install the Claude Code skill in each repo |
| `--instructions` | `install` | Also add agent instructions to `CLAUDE.md` or `AGENTS.md`. `--file` picks another file |
| `--hook` | `install` | Also add a Claude Code hook that points agents to Drift. It never blocks a tool |
| `--in <path>` | Lookups | Only look in this folder |
| `--limit <n>` | Lookups | Show at most this many lines, replacing the default size cap |
| `--all` | Lookups | Show everything |
| `--index <file>` | Lookups, `stats` | Use this index file as is, without refreshing it |
| `--json` | `bench` | Print the report as JSON, to save for `--compare` |
| `--compare <file>` | `bench` | Compare with a report saved earlier |
| `--check <percent>` | `bench` | Fail unless savings reach this percent and 90% of answers are right |

Lookups stop at a size cap, 4 KB for most and 12 KB for `show`, and say how much they left out. Results come best first: exported source, private source, tests, then fixtures, generated files and migrations.

## Supported Languages

| Language | Extensions | Symbols extracted |
|----------|-----------|-------------------|
| **TypeScript** | `.ts`, `.tsx`, `.mts`, `.cts` | functions, arrow functions, classes, methods, interfaces, types, enums, consts, namespaces |
| **JavaScript** | `.js`, `.jsx`, `.mjs`, `.cjs` | functions, arrow functions, classes, methods, consts, CommonJS exports |
| **Python** | `.py` | functions, methods, classes, constants, type aliases, decorators |
| **Rust** | `.rs` | functions, methods, structs, enums, traits, impls, type aliases, consts, statics, macros, modules |
| **SQL** | `.sql` | tables, views, functions, procedures, triggers, indexes, types, policies |

### Test Discovery

| Language | Finds | Frameworks reported |
|----------|-------|---------------------|
| TypeScript/JS | `describe`, `it`, `test` and their `.only`, `.skip`, `.each` forms, nested suites | Vitest, Jest, Mocha, Playwright, `node:test`, Cypress |
| Python | `test_*` functions, `Test*` classes, and `TestCase` subclasses, including through a base class in the same file | pytest, unittest |
| Rust | `#[test]`, `#[tokio::test]`, `#[rstest]`, and tests inside macros such as `proptest!` | cargo-test |

A JS or TS test file that names no framework takes one from the nearest `package.json`. If that doesn't settle it, the framework is reported as `unknown`.

## How It Works

Drift walks your repo in parallel and parses each file with [tree-sitter](https://tree-sitter.github.io/). Parsing rather than pattern matching means commented-out code and code inside strings are not mistaken for definitions. SQL is the exception: dialects vary too much for one grammar, so `CREATE` statements are matched by pattern, with comments removed first.

For each file it records:

1. **Symbols:** definitions with their line ranges, signatures, first doc comment line, whether they are exported, and their parent class, impl or module
2. **Imports:** what the file depends on, including re-exports, `require()` and dynamic `import()`
3. **Tests:** suites and individual cases with line numbers

### What it doesn't capture

- Types. `drift refs` finds uses by name when you ask, so unrelated things that share a common name can appear.
- Generated symbols, such as Rust macro expansions or names built at runtime.
- Local variables and functions nested inside JS or TS functions. Python nested functions are included.
- Interface and struct members.
- Non-test items inside Rust `#[cfg(test)]` modules, which are left out to keep the index small.
- Declarations caught up in syntax the grammar can't read. About 0.5% of files had some in testing. Those files are still indexed, and declarations elsewhere in them are still found.

SQL `CREATE` statements inside dollar-quoted function bodies are counted as definitions, because the pattern matcher doesn't parse those bodies.

### Ignored by default

Drift respects `.gitignore` and `.ignore` files and skips hidden directories, `node_modules`, `dist`, `build`, `target`, `vendor`, `coverage`, `venv` and similar directories. It also skips lockfiles, `*.min.js` and files over 512 KB.

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

### Claude Code

Run `drift install . --instructions` in any repo. Add `--hook` if you also want a note when an agent greps for a name the index knows or reads a large file.

### Other AI tools

Add this to your agent's instructions file:

> Before grepping or reading whole files, use Drift: `drift query <symbol>` finds a definition, `drift show <symbol>` prints it, `drift outline <file>` lists a file's definitions and imports, `drift refs <symbol>` finds uses, and `drift map` orients you in a repo. Do not read `.drift-index.json` directly, because it is large.

## Contributing

Drift's only goal is to reduce the tokens agents spend on code. See [ROADMAP.md](ROADMAP.md) for what's planned and what was ruled out, and [CONTRIBUTING.md](CONTRIBUTING.md) for how to contribute.

## License

MIT
