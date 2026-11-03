"""CLI entry point for Drift."""

import argparse
import json
import sys
import time
from pathlib import Path

from .indexer import index_repo, write_index


def main():
    parser = argparse.ArgumentParser(
        prog="drift",
        description="Index a codebase for fast agent-searchable lookups.",
    )
    sub = parser.add_subparsers(dest="command")

    idx = sub.add_parser("index", help="Generate .drift-index.json for a repo")
    idx.add_argument("path", nargs="?", default=".", help="Path to repo root (default: .)")
    idx.add_argument("-o", "--output", help="Output file path (default: <repo>/.drift-index.json)")
    idx.add_argument("--ignore-dirs", nargs="*", default=[], help="Extra directories to ignore")
    idx.add_argument("--ignore-files", nargs="*", default=[], help="Extra files to ignore")
    idx.add_argument("--compact", action="store_true", help="Compact output: strip nulls, only exported symbols in index")

    qry = sub.add_parser("query", help="Look up a symbol in the index")
    qry.add_argument("symbol", help="Symbol name to look up")
    qry.add_argument("--index", default=".drift-index.json", help="Path to index file")

    sts = sub.add_parser("stats", help="Show index statistics")
    sts.add_argument("--index", default=".drift-index.json", help="Path to index file")

    bat = sub.add_parser("batch", help="Index multiple repos under a parent directory")
    bat.add_argument("parent", help="Parent directory containing repos")
    bat.add_argument("--compact", action="store_true", help="Compact output")
    bat.add_argument("--install", action="store_true", help="Also install the Claude Code skill in each repo")

    inst = sub.add_parser("install", help="Install Claude Code skill into a repo")
    inst.add_argument("path", nargs="?", default=".", help="Path to repo root (default: .)")

    args = parser.parse_args()

    if args.command == "index":
        _cmd_index(args)
    elif args.command == "query":
        _cmd_query(args)
    elif args.command == "stats":
        _cmd_stats(args)
    elif args.command == "batch":
        _cmd_batch(args)
    elif args.command == "install":
        _cmd_install(args)
    else:
        parser.print_help()


def _cmd_index(args):
    root = Path(args.path).resolve()
    if not root.is_dir():
        print(f"Error: {root} is not a directory", file=sys.stderr)
        sys.exit(1)

    output = Path(args.output) if args.output else root / ".drift-index.json"

    start = time.time()
    index = index_repo(
        root,
        extra_ignore_dirs=set(args.ignore_dirs),
        extra_ignore_files=set(args.ignore_files),
    )
    elapsed = time.time() - start

    write_index(index, output, compact=args.compact)

    file_count = len(index["files"])
    symbol_count = sum(len(locs) for locs in index["symbol_index"].values())
    test_count = sum(
        len(c) for t in index["tests"].values() for s in t["suites"] for c in s["cases"]
    )
    size_kb = output.stat().st_size / 1024

    print(f"Indexed {file_count} files, {symbol_count} symbols, {test_count} tests")
    print(f"Languages: {', '.join(index['languages'])}")
    print(f"Output: {output} ({size_kb:.1f} KB)")
    print(f"Time: {elapsed:.2f}s")


def _cmd_query(args):
    index_path = Path(args.index)
    if not index_path.exists():
        print(f"Error: {index_path} not found. Run 'drift index' first.", file=sys.stderr)
        sys.exit(1)

    index = json.loads(index_path.read_text(encoding="utf-8"))
    symbol = args.symbol
    results = index.get("symbol_index", {}).get(symbol, [])

    if not results:
        for name, locs in index["symbol_index"].items():
            if symbol.lower() in name.lower():
                results.extend(locs)

    if not results:
        print(f"No matches for '{symbol}'")
        sys.exit(1)

    for r in results:
        exported = " (exported)" if r.get("exported") else ""
        print(f"  {r['kind']:12s} {r['file']}:{r['line']}{exported}")


def _cmd_stats(args):
    index_path = Path(args.index)
    if not index_path.exists():
        print(f"Error: {index_path} not found.", file=sys.stderr)
        sys.exit(1)

    index = json.loads(index_path.read_text(encoding="utf-8"))

    file_count = len(index["files"])
    symbol_count = sum(len(locs) for locs in index["symbol_index"].values())
    unique_symbols = len(index["symbol_index"])
    test_file_count = len(index["tests"])
    test_count = sum(
        len(c) for t in index["tests"].values() for s in t["suites"] for c in s["cases"]
    )
    total_lines = sum(f["lines"] for f in index["files"].values())

    lang_counts: dict[str, int] = {}
    for f in index["files"].values():
        lang_counts[f["lang"]] = lang_counts.get(f["lang"], 0) + 1

    print(f"Repo:       {index['repo']}")
    print(f"Generated:  {index['generated_at']}")
    print(f"Files:      {file_count}")
    print(f"Lines:      {total_lines:,}")
    print(f"Symbols:    {symbol_count} ({unique_symbols} unique)")
    print(f"Tests:      {test_count} across {test_file_count} files")
    print(f"Languages:")
    for lang, count in sorted(lang_counts.items(), key=lambda x: -x[1]):
        print(f"  {lang:12s} {count} files")


def _cmd_batch(args):
    parent = Path(args.parent).resolve()
    if not parent.is_dir():
        print(f"Error: {parent} is not a directory", file=sys.stderr)
        sys.exit(1)

    repos = sorted([d for d in parent.iterdir() if d.is_dir() and (d / ".git").exists()])
    if not repos:
        print(f"No git repos found under {parent}")
        sys.exit(1)

    print(f"Found {len(repos)} repos under {parent}\n")

    total_files = 0
    total_symbols = 0
    total_tests = 0

    for repo in repos:
        try:
            start = time.time()
            index = index_repo(repo)
            output = repo / ".drift-index.json"
            write_index(index, output, compact=args.compact)
            elapsed = time.time() - start

            fc = len(index["files"])
            sc = sum(len(locs) for locs in index["symbol_index"].values())
            tc = sum(len(c) for t in index["tests"].values() for s in t["suites"] for c in s["cases"])
            size_kb = output.stat().st_size / 1024

            total_files += fc
            total_symbols += sc
            total_tests += tc

            langs = ", ".join(index["languages"]) if index["languages"] else "none"
            print(f"  {repo.name:30s}  {fc:4d} files  {sc:5d} symbols  {tc:4d} tests  {size_kb:7.1f} KB  ({elapsed:.2f}s)  [{langs}]")

            if args.install and fc > 0:
                _do_install(repo)

        except Exception as e:
            print(f"  {repo.name:30s}  ERROR: {e}", file=sys.stderr)

    print(f"\nTotal: {total_files} files, {total_symbols} symbols, {total_tests} tests across {len(repos)} repos")


def _do_install(root: Path):
    skills_dir = root / ".claude" / "skills"
    skills_dir.mkdir(parents=True, exist_ok=True)
    skill_file = skills_dir / "drift.md"
    skill_file.write_text(_get_skill_content(), encoding="utf-8")


def _cmd_install(args):
    root = Path(args.path).resolve()
    skills_dir = root / ".claude" / "skills"
    skills_dir.mkdir(parents=True, exist_ok=True)

    skill_file = skills_dir / "drift.md"
    skill_file.write_text(_get_skill_content(), encoding="utf-8")

    print(f"Installed skill at {skill_file}")
    print("Run 'drift index' to generate the index, then agents will use it automatically.")


def _get_skill_content() -> str:
    return '''---
description: "Use the Drift index (.drift-index.json) to find symbols, files, and tests without grepping. Load this skill before searching for code."
globs:
  - "**/*.ts"
  - "**/*.tsx"
  - "**/*.js"
  - "**/*.jsx"
  - "**/*.py"
  - "**/*.rs"
  - "**/*.sql"
---

# Drift — Index-First Code Navigation

## What this is

A pre-built index at `.drift-index.json` maps every symbol (function, class, type, const, trait, struct, etc.) to its exact file:line location, plus imports and test locations.

## How to use it

**Before grepping or reading files to find where something is defined:**

1. Read `.drift-index.json` first
2. Look up the symbol in `symbol_index` — it gives you `{file, line, kind, exported}` directly
3. Go straight to that file:line instead of scanning

**For finding tests:**

1. Check the `tests` section — it maps test files to their suites and individual test cases with line numbers
2. To find tests for a specific module, look for test files whose path matches the module name

**For understanding dependencies:**

1. Each file entry has an `imports` array showing what it depends on
2. Use this to trace call chains without reading every file

## When to use Grep/Read instead

- The index is stale (check `generated_at` timestamp)
- You need the actual implementation, not just the location
- You're searching for string literals, comments, or non-symbol patterns
- The symbol was added after the last index generation

## Index structure

```
symbol_index.<name> → [{file, line, kind, exported}]  — find any symbol
files.<path>.symbols → [{name, kind, line, exported, parent}]  — browse a file's API
files.<path>.imports → [{source, names}]  — trace dependencies
tests.<path>.suites → [{name, line, cases: [{name, line}]}]  — find tests
```
'''


if __name__ == "__main__":
    main()
