---
name: drift
description: Find where functions, classes, types, tables and tests are defined, see what a file contains, and trace who imports a module, using the Drift index instead of grepping. Use before searching for code in a repo that has a .drift-index.json file.
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

`.drift-index.json` maps every symbol to its file and line range, and records each file's imports and test cases. Run these commands from the repo root before grepping or reading whole files.

| To | Run | You get |
|----|-----|---------|
| Find a definition | `drift query <symbol>` | Kind, `file:start-end`, export status. Exact names first, then substrings |
| See what a file contains | `drift outline <file>` | Its symbols with line ranges, and its imports |
| Find tests | `drift tests <text>` | Matching tests by path, suite or case name, with line numbers |
| List all test files | `drift tests` | Each file's framework and test count |
| See who imports a module | `drift importers <path or text>` | Importing files. A path like `src/lib/auth.ts` resolves relative imports and `@/` aliases |

Then read only the line range you need.

Do not read or grep `.drift-index.json` itself. It can be hundreds of kilobytes, and with `--compact` it is one line, so either returns all of it.

## When to use Grep or Read instead

- The index is missing, or older than your recent changes (`drift stats` shows when it was generated). Run `drift index . --compact` to refresh it.
- You need the implementation, not the location.
- You are searching for string literals, comments or call sites. The index records definitions and imports, not usages.
