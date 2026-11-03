"""
Index schema definition for Drift.

The .drift-index.json schema:
{
  "version": "1.0",
  "repo": "<repo-name>",
  "root": "<absolute-path>",
  "generated_at": "<ISO-8601>",
  "languages": ["typescript", "python", ...],
  "files": {
    "<relative-path>": {
      "lang": "<language>",
      "lines": <int>,
      "symbols": [
        {
          "name": "<symbol-name>",
          "kind": "function|class|type|interface|enum|const|variable|trait|impl|struct|macro|module|view|table|procedure|trigger",
          "line": <int>,
          "end_line": <int|null>,
          "exported": <bool>,
          "decorators": ["<decorator>", ...],
          "async": <bool>,
          "parent": "<parent-symbol>|null"
        }
      ],
      "imports": [
        {
          "source": "<module-or-path>",
          "names": ["<imported-name>", ...] | "*" | null
        }
      ]
    }
  },
  "tests": {
    "<relative-path>": {
      "framework": "jest|vitest|pytest|unittest|cargo-test|custom",
      "suites": [
        {
          "name": "<suite-name>",
          "line": <int>,
          "cases": [
            {"name": "<test-name>", "line": <int>}
          ]
        }
      ]
    }
  },
  "symbol_index": {
    "<symbol-name>": [
      {"file": "<relative-path>", "line": <int>, "kind": "<kind>", "exported": <bool>}
    ]
  }
}
"""

SCHEMA_VERSION = "1.0"
