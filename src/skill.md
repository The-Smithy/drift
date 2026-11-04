---
name: drift
description: Find code and docs with far fewer tokens than grepping or reading whole files: definitions, one function, a file's contents, uses, imports, and the right section of a document. Use before searching or exploring.
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
  - "**/*.md"
  - "**/*.mdx"
  - "**/*.rst"
  - "**/*.lua"
  - "**/*.gd"
  - "**/*.cs"
  - "**/*.c"
  - "**/*.h"
  - "**/*.cpp"
  - "**/*.cc"
  - "**/*.hpp"
  - "**/*.json"
  - "**/*.yaml"
  - "**/*.yml"
---

# Drift: find code, read less

Run these before grepping or reading whole files. The index builds and refreshes itself.

| To | Run |
|----|-----|
| Find a definition | `drift query <symbol>` |
| Search by words | `drift find rate limit` |
| Read one definition | `drift show <symbol>` (or `path:name`, `path:line`, `Parent.name`) |
| See how to call it | `drift sig <symbol>` |
| See what a file or long doc contains | `drift outline <file>` |
| Search docs (no words lists them) | `drift docs <words>`, then `drift show <path:start-end>` |
| Find uses | `drift refs <symbol>` |
| Find importers | `drift importers <file>` |
| Find tests | `drift tests <text>` or `drift tests --for <file>` |
| Prepare to change something | `drift context <symbol or file>` |
| Get oriented | `drift map` |
| Review a branch | `drift changes [ref]` |

Output is capped. Narrow it with `--in <path>`, or use `--limit <n>` or `--all`.

Never read `.drift-index.json`. It's large.

`drift docs` matches words, not meanings, so try others if it misses. Grep is still right for strings and comments. `drift refs` matches by name, so its hits can include other things with the same name.
