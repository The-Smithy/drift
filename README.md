<p align="center">
  <img src="logo.svg" width="180" alt="Drift logo — a blacksmith's drift punch with a heated tip">
</p>

<h1 align="center">Drift</h1>

<p align="center">
  <strong>A blacksmith's tool for pinpointing code.</strong><br>
  Fast, zero-dependency codebase indexer that gives AI agents a map before they search.
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

AI coding agents waste tokens on exploration. A typical "find where `UserController` is defined" costs thousands of tokens in grep output and file reads — just to land on a single line.

Drift builds a `.drift-index.json` that maps every symbol to its exact `file:line`. Agents read the index first, then go straight to the code. No scanning, no guessing.

**Tested across 28 repos: 4,500+ files, 36,700 symbols, 12,400 tests indexed in under 40 seconds.**

## Install

Requires Python 3.10+. No external dependencies.

```bash
pip install drift-index
```

Or run directly from a clone:

```bash
git clone https://github.com/the-smithy-io/drift.git
cd drift
python -m src index /path/to/your/repo
```

## Quick Start

**Index a repo:**

```bash
drift index .
```

**Index with compact output (64% smaller):**

```bash
drift index . --compact
```

**Index all repos under a directory:**

```bash
drift batch ~/Development --compact
```

**Look up a symbol:**

```bash
drift query UserController
#   class        src/controllers/user.ts:12 (exported)
```

**Install the Claude Code skill into a repo:**

```bash
drift install .
```

This writes a `.claude/skills/drift.md` file that teaches agents to read the index before grepping.

## Commands

| Command | Description |
|---------|-------------|
| `drift index [path]` | Index a repo, output `.drift-index.json` |
| `drift index [path] --compact` | Compact output — strips nulls, exports-only symbol index |
| `drift query <symbol>` | Look up where a symbol is defined |
| `drift stats` | Show index statistics |
| `drift batch <parent>` | Index all git repos under a directory |
| `drift batch <parent> --install` | Index all repos and install the Claude Code skill |
| `drift install [path]` | Install the Claude Code skill into a repo |

### Options

| Flag | Command | Description |
|------|---------|-------------|
| `--compact` | `index`, `batch` | Strip null fields, minify JSON, export-only symbol index (~64% smaller) |
| `-o, --output` | `index` | Custom output path (default: `.drift-index.json` in repo root) |
| `--ignore-dirs` | `index` | Extra directories to skip |
| `--ignore-files` | `index` | Extra files to skip |
| `--install` | `batch` | Also install the Claude Code skill in each repo |
| `--index` | `query`, `stats` | Path to index file (default: `.drift-index.json`) |

## Supported Languages

| Language | Extensions | Symbols Extracted |
|----------|-----------|-------------------|
| **TypeScript** | `.ts`, `.tsx` | functions, classes, interfaces, types, enums, consts, methods |
| **JavaScript** | `.js`, `.jsx`, `.mjs`, `.cjs` | functions, classes, consts, methods |
| **Python** | `.py` | functions, classes, constants, decorators |
| **Rust** | `.rs` | functions, structs, enums, traits, impls, type aliases, consts, macros, modules |
| **SQL** | `.sql` | tables, views, functions, procedures, triggers, indexes, types, policies |

### Test Framework Detection

| Language | Frameworks |
|----------|-----------|
| TypeScript/JS | Jest, Vitest, Mocha — `describe`/`it`/`test` blocks |
| Python | pytest, unittest — `test_*` functions, `Test*` classes |
| Rust | `#[test]` / `#[cfg(test)]` functions |

## How It Works

Drift uses regex-based extraction — no tree-sitter, no AST parsing, no external dependencies. It walks your repo, reads each file, and extracts:

1. **Symbols** — function/class/type/const definitions with their line numbers
2. **Imports** — what each file depends on (module paths and imported names)
3. **Tests** — test suites and individual test cases with line numbers
4. **Symbol index** — a reverse lookup from symbol name to `{file, line, kind}`

The output is a single `.drift-index.json` file. Compact mode strips null fields and limits the symbol index to exported symbols, cutting size by ~64%.

### What regex misses

Regex handles ~90% of symbols accurately. It misses:

- Destructured exports (`export const { a, b } = obj`)
- Re-exports (`export * from`)
- Dynamically generated symbols
- Macro-expanded items in Rust
- `__all__` contents in Python

These edge cases rarely matter for the primary use case: pointing agents to the right file and line.

### Ignored by default

Drift respects `.gitignore` and skips common non-source directories: `node_modules`, `dist`, `build`, `.venv`, `target`, `__pycache__`, and 20+ others. Files over 512 KB are skipped.

## Index Schema

```json
{
  "version": "1.0",
  "repo": "my-project",
  "generated_at": "2026-11-03T14:00:00Z",
  "languages": ["typescript", "python"],
  "files": {
    "src/auth.ts": {
      "lang": "typescript",
      "lines": 142,
      "symbols": [
        {"name": "AuthService", "kind": "class", "line": 12, "exported": true},
        {"name": "validateToken", "kind": "function", "line": 45, "exported": true, "parent": "AuthService"}
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
    "AuthService": [{"file": "src/auth.ts", "line": 12, "kind": "class", "exported": true}],
    "validateToken": [{"file": "src/auth.ts", "line": 45, "kind": "function", "exported": true}]
  }
}
```

## For Agents

See [AGENTS.md](AGENTS.md) for detailed integration instructions.

**The short version:** read `.drift-index.json` before grepping. Look up symbols in `symbol_index`, find tests in `tests`, trace imports in `files.<path>.imports`.

### Claude Code

Run `drift install .` in any repo. This adds a skill file that Claude Code loads automatically when working on supported file types.

### Other AI tools

Add this to your agent's system prompt or instructions file:

> Before searching for code with grep or reading files to find definitions, check `.drift-index.json` in the repo root. The `symbol_index` section maps symbol names directly to `{file, line, kind}`. Go to that location instead of scanning.

## License

MIT
