"""Extract symbols from SQL files."""

import re

_CREATE_TABLE = re.compile(
    r"CREATE\s+(?:OR\s+REPLACE\s+)?(?:TEMP(?:ORARY)?\s+)?TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?(?P<name>[\w.\"]+)",
    re.IGNORECASE | re.MULTILINE,
)

_CREATE_VIEW = re.compile(
    r"CREATE\s+(?:OR\s+REPLACE\s+)?(?:MATERIALIZED\s+)?VIEW\s+(?:IF\s+NOT\s+EXISTS\s+)?(?P<name>[\w.\"]+)",
    re.IGNORECASE | re.MULTILINE,
)

_CREATE_FUNC = re.compile(
    r"CREATE\s+(?:OR\s+REPLACE\s+)?FUNCTION\s+(?P<name>[\w.\"]+)",
    re.IGNORECASE | re.MULTILINE,
)

_CREATE_PROC = re.compile(
    r"CREATE\s+(?:OR\s+REPLACE\s+)?PROC(?:EDURE)?\s+(?P<name>[\w.\"]+)",
    re.IGNORECASE | re.MULTILINE,
)

_CREATE_TRIGGER = re.compile(
    r"CREATE\s+(?:OR\s+REPLACE\s+)?TRIGGER\s+(?P<name>[\w.\"]+)",
    re.IGNORECASE | re.MULTILINE,
)

_CREATE_INDEX = re.compile(
    r"CREATE\s+(?:UNIQUE\s+)?INDEX\s+(?:CONCURRENTLY\s+)?(?:IF\s+NOT\s+EXISTS\s+)?(?P<name>[\w.\"]+)",
    re.IGNORECASE | re.MULTILINE,
)

_CREATE_TYPE = re.compile(
    r"CREATE\s+TYPE\s+(?P<name>[\w.\"]+)",
    re.IGNORECASE | re.MULTILINE,
)

_CREATE_POLICY = re.compile(
    r"CREATE\s+POLICY\s+(?P<name>[\w.\"]+)",
    re.IGNORECASE | re.MULTILINE,
)

_ALTER_TABLE = re.compile(
    r"ALTER\s+TABLE\s+(?:ONLY\s+)?(?:IF\s+EXISTS\s+)?(?P<name>[\w.\"]+)",
    re.IGNORECASE | re.MULTILINE,
)


def _clean_name(name: str) -> str:
    return name.strip('"').strip("'").strip("`")


def extract_sql(content: str, path: str) -> dict:
    lines = content.split("\n")
    line_count = len(lines)
    symbols = []

    def _line_num(match):
        return content[:match.start()].count("\n") + 1

    patterns = [
        (_CREATE_TABLE, "table"),
        (_CREATE_VIEW, "view"),
        (_CREATE_FUNC, "function"),
        (_CREATE_PROC, "procedure"),
        (_CREATE_TRIGGER, "trigger"),
        (_CREATE_INDEX, "const"),
        (_CREATE_TYPE, "type"),
        (_CREATE_POLICY, "const"),
    ]

    for pattern, kind in patterns:
        for m in pattern.finditer(content):
            symbols.append({
                "name": _clean_name(m.group("name")),
                "kind": kind,
                "line": _line_num(m),
                "end_line": None,
                "exported": True,
                "async": False,
                "parent": None,
            })

    symbols.sort(key=lambda s: s["line"])

    return {
        "lang": "sql",
        "lines": line_count,
        "symbols": symbols,
        "imports": [],
        "tests": None,
    }
