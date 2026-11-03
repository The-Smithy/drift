---
name: drift
description: Find where functions, classes, types, tables and other symbols are defined using the Drift index instead of grepping. Use before searching for code in a repo that has a .drift-index.json file.
paths:
  - "**/*.ts"
  - "**/*.tsx"
  - "**/*.mts"
  - "**/*.cts"
  - "**/*.js"
  - "**/*.jsx"
  - "**/*.mjs"
  - "**/*.cjs"
  - "**/*.py"
  - "**/*.rs"
  - "**/*.sql"
---

# Drift: index-first code navigation

`.drift-index.json` maps every symbol to its file and line range, and records each file's imports and test cases.

## Finding a definition

Run this before grepping for where something is defined:

```bash
drift query <symbol>
```

It prints the kind, `file:start-end` and whether the symbol is exported. Exact names match first, then case-insensitive substrings. Read only that line range of the file.

## Do not read the index file directly

The index can be hundreds of kilobytes, and with `--compact` it is one line, so reading or grepping it returns all of it. Use `drift query`, or a tool that extracts one key:

```bash
jq '.files["src/auth.ts"]' .drift-index.json
jq '.tests | keys' .drift-index.json
```

If `jq` isn't installed, use a short script in any language that parses the JSON and prints only the key you need.

## Index structure

```
files.<path>.symbols -> [{name, kind, line, end_line, exported, parent}]
files.<path>.imports -> [{source, names}]
tests.<path>.suites  -> [{name, line, cases: [{name, line}]}]
symbol_index.<name>  -> [{file, line, end_line, kind, exported}]
```

## When to use Grep or Read instead

- The index is missing, or older than your recent changes (check `generated_at`). Run `drift index . --compact` to refresh it.
- You need the implementation, not the location.
- You are searching for string literals, comments or call sites. The index records definitions, not usages.
