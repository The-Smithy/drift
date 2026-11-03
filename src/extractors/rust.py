"""Extract symbols and imports from Rust files."""

import re

_FUNC = re.compile(
    r"^(?P<pub>pub(?:\([\w:]+\))?\s+)?(?P<async>async\s+)?(?:unsafe\s+)?(?:extern\s+\"[^\"]*\"\s+)?fn\s+(?P<name>\w+)",
    re.MULTILINE,
)

_STRUCT = re.compile(
    r"^(?P<pub>pub(?:\([\w:]+\))?\s+)?struct\s+(?P<name>\w+)",
    re.MULTILINE,
)

_ENUM = re.compile(
    r"^(?P<pub>pub(?:\([\w:]+\))?\s+)?enum\s+(?P<name>\w+)",
    re.MULTILINE,
)

_TRAIT = re.compile(
    r"^(?P<pub>pub(?:\([\w:]+\))?\s+)?trait\s+(?P<name>\w+)",
    re.MULTILINE,
)

_IMPL = re.compile(
    r"^impl(?:<[^>]*>)?\s+(?:(?P<trait>\w+)\s+for\s+)?(?P<name>\w+)",
    re.MULTILINE,
)

_TYPE_ALIAS = re.compile(
    r"^(?P<pub>pub(?:\([\w:]+\))?\s+)?type\s+(?P<name>\w+)",
    re.MULTILINE,
)

_CONST_STATIC = re.compile(
    r"^(?P<pub>pub(?:\([\w:]+\))?\s+)?(?:const|static)\s+(?P<name>[A-Z_]\w*)\s*:",
    re.MULTILINE,
)

_MACRO = re.compile(
    r"^(?:(?:#\[macro_export\]\s*\n\s*)?macro_rules!\s+(?P<name>\w+))",
    re.MULTILINE,
)

_MOD = re.compile(
    r"^(?P<pub>pub(?:\([\w:]+\))?\s+)?mod\s+(?P<name>\w+)\s*;",
    re.MULTILINE,
)

_USE = re.compile(
    r"^(?:pub\s+)?use\s+(?P<path>[^;]+);",
    re.MULTILINE,
)

_TEST_ATTR = re.compile(r"#\[test\]")
_TEST_MOD = re.compile(r"#\[cfg\(test\)\]")
_TEST_FN = re.compile(r"^\s*(?:async\s+)?fn\s+(?P<name>\w+)", re.MULTILINE)


def extract_rust(content: str, path: str) -> dict:
    lines = content.split("\n")
    line_count = len(lines)
    symbols = []
    imports = []

    current_impl = None

    for i, line in enumerate(lines, 1):
        stripped = line.lstrip()

        m = _IMPL.match(line)
        if m:
            trait_name = m.group("trait")
            impl_name = m.group("name")
            label = f"{trait_name} for {impl_name}" if trait_name else impl_name
            current_impl = impl_name
            symbols.append({
                "name": label,
                "kind": "impl",
                "line": i,
                "end_line": None,
                "exported": False,
                "async": False,
                "parent": None,
            })
            continue

        m = _FUNC.match(line)
        if m:
            indent = len(line) - len(stripped)
            parent = current_impl if indent > 0 and current_impl else None
            symbols.append({
                "name": m.group("name"),
                "kind": "function",
                "line": i,
                "end_line": None,
                "exported": bool(m.group("pub")),
                "async": bool(m.group("async")),
                "parent": parent,
            })
            continue

        m = _STRUCT.match(line)
        if m:
            symbols.append({
                "name": m.group("name"),
                "kind": "struct",
                "line": i,
                "end_line": None,
                "exported": bool(m.group("pub")),
                "async": False,
                "parent": None,
            })
            continue

        m = _ENUM.match(line)
        if m:
            symbols.append({
                "name": m.group("name"),
                "kind": "enum",
                "line": i,
                "end_line": None,
                "exported": bool(m.group("pub")),
                "async": False,
                "parent": None,
            })
            continue

        m = _TRAIT.match(line)
        if m:
            symbols.append({
                "name": m.group("name"),
                "kind": "trait",
                "line": i,
                "end_line": None,
                "exported": bool(m.group("pub")),
                "async": False,
                "parent": None,
            })
            continue

        m = _TYPE_ALIAS.match(line)
        if m:
            symbols.append({
                "name": m.group("name"),
                "kind": "type",
                "line": i,
                "end_line": None,
                "exported": bool(m.group("pub")),
                "async": False,
                "parent": None,
            })
            continue

        m = _CONST_STATIC.match(line)
        if m:
            symbols.append({
                "name": m.group("name"),
                "kind": "const",
                "line": i,
                "end_line": None,
                "exported": bool(m.group("pub")),
                "async": False,
                "parent": None,
            })
            continue

        m = _MACRO.match(line)
        if m:
            symbols.append({
                "name": m.group("name"),
                "kind": "macro",
                "line": i,
                "end_line": None,
                "exported": True,
                "async": False,
                "parent": None,
            })
            continue

        m = _MOD.match(line)
        if m:
            symbols.append({
                "name": m.group("name"),
                "kind": "module",
                "line": i,
                "end_line": None,
                "exported": bool(m.group("pub")),
                "async": False,
                "parent": None,
            })
            continue

        if stripped == "}" and len(line) - len(stripped) == 0:
            current_impl = None

    for m in _USE.finditer(content):
        raw = m.group("path").strip()
        brace = raw.find("{")
        if brace != -1:
            base = raw[:brace].rstrip(":").rstrip()
            items_str = raw[brace + 1:raw.find("}")].strip()
            names = [n.strip() for n in items_str.split(",") if n.strip()]
            imports.append({"source": base, "names": names})
        else:
            parts = raw.split("::")
            if len(parts) > 1:
                imports.append({"source": "::".join(parts[:-1]), "names": [parts[-1]]})
            else:
                imports.append({"source": raw, "names": None})

    tests = _extract_tests_rust(content, path)

    return {
        "lang": "rust",
        "lines": line_count,
        "symbols": symbols,
        "imports": imports,
        "tests": tests,
    }


def _extract_tests_rust(content: str, path: str) -> dict | None:
    test_attrs = list(_TEST_ATTR.finditer(content))
    if not test_attrs:
        return None

    def _line_num(match):
        return content[:match.start()].count("\n") + 1

    suites = []
    cases = []
    for attr in test_attrs:
        pos = attr.end()
        rest = content[pos:]
        m = _TEST_FN.search(rest)
        if m:
            cases.append({"name": m.group("name"), "line": _line_num(attr) + content[pos:pos + m.start()].count("\n")})

    if cases:
        has_test_mod = bool(_TEST_MOD.search(content))
        suite_name = "tests" if has_test_mod else "(module)"
        suites.append({"name": suite_name, "line": cases[0]["line"], "cases": cases})

    return {"framework": "cargo-test", "suites": suites} if suites else None
