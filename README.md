<p align="center">
  <img src="logo.svg" width="180" alt="Drift logo — scattered dots converging to a focal point">
</p>

<h1 align="center">Drift</h1>

<p align="center">
  <strong>A blacksmith's tool for pinpointing code.</strong><br>
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

Drift builds a `.drift-index.json` that maps every symbol to its exact file and line range. An agent runs one short lookup, then reads only those lines.

```console
$ drift query UserController
  class        UserController  src/controllers/user.ts:12-140 (exported)
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

**Index a repo:**

```bash
drift index . --compact
```

**Look up a symbol:**

```bash
drift query UserController
```

**Index every repo under a directory:**

```bash
drift batch ~/Development --compact
```

**Teach Claude Code to use the index in a repo:**

```bash
drift install .
```

This writes `.claude/skills/drift/SKILL.md`, which tells agents to run `drift query` before grepping.

Add `.drift-index.json` to your `.gitignore`. It contains an absolute path and goes stale quickly.

## Commands

| Command | Description |
|---------|-------------|
| `drift index [path]` | Index a repo and write `.drift-index.json` |
| `drift query <symbol>` | Find where a symbol is defined. Exact names match first, then case-insensitive substrings |
| `drift stats` | Show index statistics |
| `drift batch <parent>` | Index every git repo directly under a directory |
| `drift install [path]` | Install the Claude Code skill into a repo |

### Options

| Flag | Command | Description |
|------|---------|-------------|
| `--compact` | `index`, `batch` | Strip default values, minify, and limit `symbol_index` to exported symbols (about 67% smaller) |
| `-o, --output` | `index` | Custom output path (default: `.drift-index.json` in the repo root) |
| `--ignore-dirs` | `index` | Extra directory names to skip |
| `--ignore-files` | `index` | Extra file names to skip |
| `--install` | `batch` | Also install the Claude Code skill in each repo |
| `--index` | `query`, `stats` | Path to the index file (default: `.drift-index.json`) |

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

1. **Symbols:** definitions with their line ranges, whether they are exported, and their parent class, impl or module
2. **Imports:** what the file depends on, including re-exports, `require()` and dynamic `import()`
3. **Tests:** suites and individual cases with line numbers

### What it doesn't capture

- Usages. The index records where things are defined, not where they are called.
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

**The short version:** run `drift query <symbol>` before grepping, then read only the line range it prints. Don't read the index file itself. A large repo's index runs to hundreds of kilobytes, and with `--compact` it is a single line.

### Claude Code

Run `drift install .` in any repo. Claude Code will then load the skill when working on supported file types.

### Other AI tools

Add this to your agent's instructions file:

> Before searching for where something is defined, run `drift query <symbol>`. It prints the kind, `file:start-end` and export status. Read only that line range. Do not read `.drift-index.json` directly, because it is large.

## License

MIT
