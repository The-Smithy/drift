# Drift: Agent Integration Guide

This document explains how AI coding agents should use Drift to reduce token usage and find code faster.

## The one rule: run commands, don't read the index

Drift keeps a `.drift-index.json` file in the repo root. It can be hundreds of kilobytes, and with `--compact` it's a single line, so reading or grepping it costs more than the search you were avoiding. Use the commands below. Each one prints only what you asked for.

You don't need to manage the index. A lookup builds it if it's missing and refreshes it if any file has changed.

## Which command to use

| You want to | Run |
|-------------|-----|
| Find where something is defined | `drift query <symbol>` |
| Search by words when you don't know the name | `drift find rate limit` |
| Read one definition | `drift show <symbol>` |
| See how to call something | `drift sig <symbol>` |
| See what a file contains | `drift outline <file>` |
| Find where something is used | `drift refs <symbol>` |
| Find what imports a file | `drift importers <file>` |
| Find tests | `drift tests <text>` or `drift tests --for <file>` |
| Prepare to change something | `drift context <symbol or file>` |
| Get oriented in a repo | `drift map` |
| Review a branch or your changes | `drift changes [ref]` |

**Use Grep or Read instead when** you need strings or comments, or when you've already found the lines you need and want the surrounding code.

## Output limits

Every lookup stops at a size cap and ends with a line saying how much was left out:

| Commands | Cap |
|----------|-----|
| `query`, `find`, `sig`, `outline`, `refs`, `tests`, `importers`, `changes` | 4 KB |
| `context`, `map` | 6 KB |
| `show` | 12 KB |

Results come best first: exported source code, then private source code, then tests, then fixtures, generated files, documents and migrations. Newer migrations come before older ones.

| Flag | What it does |
|------|--------------|
| `--in <path>` | Only look in this folder |
| `--limit <n>` | Show at most this many lines, replacing the cap |
| `--all` | Show everything |
| `--index <file>` | Use this index file as is, without refreshing it |

## Using each command

### Finding a definition

```console
$ drift query UserController
class UserController  src/controllers/user.ts:12-140 (exported)
```

Each line shows the kind, name, `file:start-end`, whether it's exported, and its parent if it has one.

Exact names match first, then case-insensitive substrings. A miss suggests close names and exits with code 1. Pass several names to save a round trip, as in `drift query login logout`. Private symbols are found too.

### Searching by words

```console
$ drift find rate limit
function enforceRateLimit  src/lib/limits.ts:40-71 (exported)  # Rejects a request over its quota.
```

Words can be in any order. They match whole words in a name first, then word prefixes, then doc comments and file paths. Every word has to match somewhere.

### Reading one definition

```console
$ drift show verifyToken
function verifyToken  src/auth.ts:2-4 (exported)
export function verifyToken(token: string): boolean {
  return token.length > 0;
}
```

If a name is defined in several places, you get the best match and a list of the others. Choose one with `Parent.name` or `path:name`, which also work with `sig` and `query`.

### Seeing how to call something

```console
$ drift sig verifyToken
src/auth.ts:2-4 (exported)
  function verifyToken(token: string): boolean
  Checks a session token.
```

The second line is the signature, cut off at 200 characters. The third is the first line of its doc comment, if it has one. This is usually all you need to call a function, at a fraction of the size of its body.

### Seeing what a file contains

```console
$ drift outline src/controllers/user.ts
src/controllers/user.ts  typescript, 142 lines
  class UserController  12-140 (exported)
  function getUser  45-60 (exported) in UserController
Imports:
  ../models/User: User, UserInput
  ../lib/auth: requireAuth
```

Add `--sig` to show signatures instead of names.

The path can be absolute, relative to the repo root, or a trailing part such as `controllers/user.ts`. If a trailing part fits several files, the command lists them and exits with code 1.

This works on Markdown too, and it's the cheapest way into a long document: outline it, then read one section with `drift show docs/guide.md:Install`.

### Finding uses

```console
$ drift refs verifyToken
3 references in 2 files
src/app.ts
  5 handleRequest: return verifyToken(token);
tests/auth.test.ts
  3 return verifyToken('x');
```

Each line shows the line number, the enclosing definition and the code. Comments, strings, import lines and the definition itself are left out.

**This matches by name, not by type.** A common name like `get` can include unrelated things that share it. A name used only inside a string isn't found, so use Grep for those.

### Finding what imports a file

```console
$ drift importers src/lib/auth.ts
src/app/page.ts  ../lib/auth: requireAuth
src/app/login.ts  @/lib/auth: signIn, signOut
```

Prefer passing a file path:

- **JS and TS:** resolves relative imports and `@/` or `~/` aliases, so `src/lib/auth.ts` won't match an import of `./oauth`.
- **Python:** resolves module paths, including `from . import sibling`.
- **Rust:** resolves `use` paths through `crate`, `self`, `super` and workspace crates, and also lists `mod` declarations.

Text that isn't an indexed file matches any import source containing it, as in `drift importers forge_core`.

An empty result doesn't always mean a file is unused. Other aliases aren't resolved, Rust crate names are inferred from the crate's directory, and Rust code that calls `crate::model::load()` with no `use` isn't an import. Use `drift refs` or Grep when it matters.

### Finding tests

```console
$ drift tests --for src/lib/auth.ts
tests/auth.test.ts  vitest  12 tests

$ drift tests duplicate
tests/controllers/user.test.ts  vitest
  UserController  (line 5)
    28  rejects duplicate email
```

`--for` finds a file's own inline tests, tests that import it, and tests named after it. With no arguments, `drift tests` summarizes every test file.

Text is matched case-insensitively. A match on a file path shows the whole file, a match on a suite name shows the whole suite, and a match on a case name shows just that case.

The framework is one of `vitest`, `jest`, `mocha`, `playwright`, `node-test`, `cypress`, `pytest`, `unittest`, `cargo-test` or `unknown`.

### Preparing to change something

```console
$ drift context verifyToken
src/auth.ts:2-4 (exported)
  function verifyToken(token: string): boolean
  Checks a session token.
Used in 3 places:
  src/app.ts:5 handleRequest
  ...
Imported by 2 files:
  src/app.ts
  ...
Tests:
  tests/auth.test.ts  vitest  1 test
```

Given a file, it lists the file's exported signatures, importers and tests. Long lists stop at 8 items and name the command that shows the rest.

### Getting oriented

```console
$ drift map
my-app  331 files, 27524 lines, 1706 symbols
Languages: typescript 308, sql 15, javascript 8
Folders:
  src/app  149 files
  ...
Entry points:
  src/theme/index.ts
Most imported:
  src/lib/session.ts  60 importers: Viewer, getSession, requireUser
  ...
Tests: 6 files (vitest)
```

Run this first in an unfamiliar repo, instead of exploring with `ls` and opening files.

### Reviewing changes

```console
$ drift changes master
src/auth.ts
  function verifyToken  2-4 (exported)
src/old.ts  deleted
src/new.ts  new, untracked
```

This lists the definitions touched since a git ref, or since `HEAD` by default, including uncommitted work. Definitions that were removed from a file that still exists aren't listed, so read the diff when that matters.

## Symbol kinds

| Kind | Languages | Description |
|------|-----------|-------------|
| `function` | All | Functions, methods, and JS or TS consts holding a function |
| `class` | TS/JS, Python | Class definitions |
| `interface` | TS | Interface definitions |
| `type` | TS, Rust, Python, SQL | Type aliases, `CREATE TYPE` |
| `enum` | TS, Rust | Enum definitions |
| `const` | TS/JS, Python, Rust, SQL | Constants and variables, Rust statics, Python `ALL_CAPS` names, SQL indexes and policies |
| `struct` | Rust | Structs and unions |
| `trait` | Rust | Trait definitions |
| `impl` | Rust | Impl blocks, named `Type` or `Trait for Type` |
| `macro` | Rust | `macro_rules!` definitions |
| `module` | Rust, TS | Modules and namespaces |
| `heading` | Markdown | A heading and the section under it. Never counted as exported |
| `table` | SQL | `CREATE TABLE` |
| `view` | SQL | `CREATE VIEW`, including materialized views |
| `procedure` | SQL | `CREATE PROCEDURE` |
| `trigger` | SQL | `CREATE TRIGGER` |

Schema-qualified SQL names keep their schema, such as `public.users`. To find one by its bare name, rely on the substring fallback in `drift query`.

## What `exported` means

| Language | Exported when |
|----------|---------------|
| TS/JS | It has `export`, is named in `export { }` or `export default`, or is assigned through CommonJS `exports`. Methods count when their class is exported and they aren't `private`, `protected` or `#private` |
| Python | Its name has no leading underscore, or it is listed in `__all__` when the module defines one. Functions nested in functions never count |
| Rust | It has any `pub` modifier, including `pub(crate)`. Items in a trait follow the trait. Trait impl methods don't count |
| SQL | Always |

## Update notices

A lookup may print one line to stderr saying a newer release of Drift is available. It appears at most once a day and doesn't affect the answer on stdout. Mention it to the user and carry on. Don't update Drift unless they ask.

## Integration examples

### Any coding agent

```bash
drift install . --instructions
```

- The skill goes in `.claude/skills/drift/` and `.agents/skills/drift/`. Most tools read one of these and load the skill only when it's relevant, which costs the fewest tokens.
- The instructions go in `AGENTS.md`, and also in `CLAUDE.md` and `GEMINI.md` if the repo has them. Running it again updates the section rather than adding another. Use `--file SITE.md` to put it in one other file instead.
- The index is added to `.gitignore`.

Kiro, Junie, Continue and Aider get their own files when the repo already uses them. Add `--agent kiro`, or `--agent all`, to set them up anyway. See the [README](README.md#for-agents) for which tool reads what.

### Claude Code

```bash
drift install . --hook
```

`--hook` adds a Claude Code hook to `.claude/settings.json`. When an agent searches for a name the index knows, or reads a whole file over 8 KB, the hook adds a short note pointing to Drift. It never blocks a tool, and if anything goes wrong it stays silent. It needs `drift` on the `PATH`.

### Anything else

Add this to your agent's instructions:

```
Before grepping or reading whole files, use Drift:
  drift query <symbol>     finds a definition
  drift show <symbol>      prints one definition
  drift outline <file>     lists a file's definitions and imports
  drift refs <symbol>      finds uses
  drift map                orients you in a repo
Do not read .drift-index.json directly, because it is large.
```

### Programmatic access

For scripts that need more than the commands offer, load the file in your own code. The index format is defined in `src/model.rs`.

```python
import json

with open(".drift-index.json") as f:
    index = json.load(f)

file_data = index["files"]["src/main.ts"]
exports = [s for s in file_data["symbols"] if s.get("exported")]
```
