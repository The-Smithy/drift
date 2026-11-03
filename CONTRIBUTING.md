# Contributing to Drift

Drift is a small utility with one purpose: **reduce the tokens an AI agent spends finding and understanding code.** That purpose decides what gets in. A useful feature that doesn't save tokens belongs in a different tool.

Check [ROADMAP.md](ROADMAP.md) first, including its "Decided against" list. Before starting anything sizable, open an issue with the problem and the numbers you expect, so nobody builds something that won't be accepted.

## The bar for a change

Every feature has to answer yes to all of these:

1. **Does it save tokens?** It reduces what an agent reads to do a real task, compared with what the agent does today.
2. **Can you show the numbers?** See [Measuring a change](#measuring-a-change).
3. **Does it stay a utility?** It's a local, one-shot command with no server, account, background process or network access.
4. **Does it earn its size?** Any new dependency, flag or line of default output has to pay for itself.

These changes need no token numbers: bug fixes, accuracy fixes, and build or documentation work.

## Measuring a change

Until the benchmark suite from Phase 0 exists, compare output sizes by hand on at least one real repo, and put the results in your PR:

```bash
drift query requireAuth | wc -c
git grep -n requireAuth | wc -c
wc -c src/lib/auth.ts
```

| Task | Without this change | With it |
|------|--------------------|---------|
| Find a function's definition | 10,861 bytes (`git grep`) | 77 bytes |

Compare against what an agent would really do, which is usually `grep` or reading the file. Count bytes, and use 4 bytes per token if you convert. Include your worst case as well as your best, such as a very common name in a large repo.

## Rules for output

Output is the product, because every byte printed goes into an agent's context.

- **Default to the smallest useful answer.** Extra detail goes behind a flag.
- **Never print unbounded output.** If a result can grow with repo size, it needs a limit and a line saying what was left out.
- **Skip decoration.** No banners, colors, progress bars or repeated explanations. Don't repeat a file path on every line when one grouping will do.
- **Make the next step obvious.** Print `file:start-end`, so an agent can read just those lines.
- **Make misses cheap.** Say clearly when nothing matched and exit with code 1, so an agent doesn't retry blindly.
- **Treat output as an interface.** Agents and scripts depend on the format, so changing it is a change worth calling out in the PR.

## Accuracy comes first

A wrong answer costs more than a long one. If Drift wrongly says nothing imports a file, an agent may make an unsafe change based on that.

- Compare new lookups against an independent source, such as `git grep`, on a real repo. Explain every difference.
- Include a test for any false result you fix.
- Document what a command can miss. If an empty result doesn't prove something is absent, say so in `AGENTS.md`.

## Instructions cost tokens too

`src/skill.md` and the instructions in users' repos are loaded by agents, some of them in every session. Keep them short, and remove a line when a feature makes it unnecessary. Any addition to the skill has to justify its cost.

## Getting set up

You need a recent stable [Rust](https://rustup.rs) and a C compiler, because the tree-sitter grammars are written in C.

```bash
git clone https://github.com/The-Smithy/drift
cd drift
cargo test
```

| Path | What's there |
|------|--------------|
| `src/main.rs` | Commands and flags |
| `src/index.rs` | Walking a repo, and reading and writing the index |
| `src/lookup.rs` | `query`, `outline`, `tests` and `importers` |
| `src/model.rs` | The index format |
| `src/allocator.rs` | mimalloc, for the musl build only |
| `src/extract/` | One extractor per language |
| `src/skill.md` | The skill written by `drift install` |
| `tests/cli.rs` | Tests that run the real binary |

## Adding a language

1. Check real usage first. Roadmap priorities come from what agents actually read, so say why this language matters.
2. Add a grammar crate and a file in `src/extract/`, following an existing extractor. Register it in `src/extract/mod.rs`, in both `is_supported` and `extract`.
3. Use tree-sitter unless a grammar can't do the job, so commented-out code and strings aren't mistaken for definitions. SQL is the one exception today.
4. Add tests for each kind of symbol, for exports, for imports and for test discovery. Include the cases that are easy to get wrong.
5. Run it on a real repo in that language and check the symbols against the source.
6. Update the language tables in `README.md` and `AGENTS.md`.

Note the change in binary size in your PR. Each grammar adds to it.

## Changing the index format

The format is defined in `src/model.rs`. Prefer adding optional fields, and bump `SCHEMA_VERSION` in that file for any change. Old indexes need to keep loading, or Drift has to rebuild them.

## Before opening a PR

CI runs these on Linux, macOS and Windows, and all of them must pass:

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

CI also builds the release archives when a PR changes `Cargo.toml`, `Cargo.lock` or the release workflow. That includes the static Linux (musl) build. Code that only compiles on one target can pass locally and still fail there.

In your PR, please include:

- The problem, and your before-and-after numbers
- What you checked on a real repo, and what you couldn't check
- Any change to output format or index format

Keep commits focused, and explain why in the message rather than what. Comments in code should explain only what the code can't, such as a hidden constraint.

Using an AI agent to write your change is fine and fairly fitting. You're still responsible for the result, so run the numbers yourself and make sure you can explain every difference.

## Dependencies

Drift ships as a single binary with no runtime dependencies, and that won't change. A new crate needs a clear reason. Features that need a network connection, a service or a background process don't fit the project.

## Releasing

For maintainers:

1. Set the new version in `Cargo.toml`, and merge that to `master`.
2. Push a matching tag, such as `v0.3.0`. A tag containing `-`, such as `v0.3.0-rc.1`, is published as a prerelease.

The Release workflow then builds every target, checks each packaged binary, and publishes the archives with checksums. It fails if the tag doesn't match the version in `Cargo.toml`.

## License

By contributing, you agree that your work is licensed under the [MIT License](LICENSE).
