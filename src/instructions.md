## Finding code: use Drift first

Before grepping or reading whole files, use [Drift](https://github.com/The-Smithy/drift). It costs far fewer tokens.

- `drift query <symbol>` or `drift find <words>`: where something is defined
- `drift show <symbol>` or `drift sig <symbol>`: its source, or just its signature
- `drift outline <file>`: a file's definitions and imports
- `drift refs <symbol>`, `drift importers <file>`, `drift tests --for <file>`: what uses it
- `drift context <symbol or file>`: all of the above before changing something
- `drift map`: orientation in a new repo

Never read `.drift-index.json`. Grep is still right for strings and comments. If `drift` isn't installed: `cargo install --git https://github.com/The-Smithy/drift`.
