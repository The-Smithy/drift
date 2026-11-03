"""Extract symbols and imports from TypeScript/JavaScript files."""

import re

# --- Symbol patterns ---

_FUNC = re.compile(
    r"^(?P<exp>export\s+(?:default\s+)?)?(?P<async>async\s+)?function\s+(?P<name>\w+)",
    re.MULTILINE,
)

_CLASS = re.compile(
    r"^(?P<exp>export\s+(?:default\s+)?)?(?:abstract\s+)?class\s+(?P<name>\w+)",
    re.MULTILINE,
)

_IFACE = re.compile(
    r"^(?P<exp>export\s+(?:default\s+)?)?interface\s+(?P<name>\w+)",
    re.MULTILINE,
)

_TYPE = re.compile(
    r"^(?P<exp>export\s+(?:default\s+)?)?type\s+(?P<name>\w+)\s*(?:<[^>]*>)?\s*=",
    re.MULTILINE,
)

_ENUM = re.compile(
    r"^(?P<exp>export\s+(?:default\s+)?)?(?:const\s+)?enum\s+(?P<name>\w+)",
    re.MULTILINE,
)

_CONST = re.compile(
    r"^(?P<exp>export\s+(?:default\s+)?)?(?:const|let|var)\s+(?P<name>[A-Za-z_$]\w*)\s*(?::\s*[^=]+)?\s*=",
    re.MULTILINE,
)

_METHOD = re.compile(
    r"^\s+(?P<async>async\s+)?(?P<name>\w+)\s*\(",
    re.MULTILINE,
)

# --- Import patterns ---

_IMPORT_FROM = re.compile(
    r"""import\s+(?:\{[^}]*\}|[^;{]*?)\s+from\s+['"](?P<src>[^'"]+)['"]""",
    re.MULTILINE,
)

_IMPORT_SIDE = re.compile(
    r"""^import\s+['"](?P<src>[^'"]+)['"]""",
    re.MULTILINE,
)

_REQUIRE = re.compile(
    r"""require\(\s*['"](?P<src>[^'"]+)['"]\s*\)""",
)

_IMPORT_NAMES = re.compile(
    r"""import\s+\{(?P<names>[^}]*)\}\s+from\s+['"](?P<src>[^'"]+)['"]""",
    re.MULTILINE,
)

_IMPORT_DEFAULT = re.compile(
    r"""import\s+(?P<name>[A-Za-z_$]\w*)\s+from\s+['"](?P<src>[^'"]+)['"]""",
    re.MULTILINE,
)

_IMPORT_STAR = re.compile(
    r"""import\s+\*\s+as\s+\w+\s+from\s+['"](?P<src>[^'"]+)['"]""",
    re.MULTILINE,
)

# --- Test patterns ---

_TEST_DESCRIBE = re.compile(
    r"""(?:describe|suite)\s*\(\s*['"`](?P<name>[^'"`]+)['"`]""",
)

_TEST_CASE = re.compile(
    r"""(?:it|test)\s*\(\s*['"`](?P<name>[^'"`]+)['"`]""",
)


def extract_typescript(content: str, path: str) -> dict:
    lines = content.split("\n")
    line_count = len(lines)
    symbols = []
    imports = []

    current_class = None
    class_indent = -1

    for i, line in enumerate(lines, 1):
        stripped = line.lstrip()
        indent = len(line) - len(stripped)

        if current_class and indent <= class_indent and stripped and not stripped.startswith("//"):
            current_class = None
            class_indent = -1

        m = _FUNC.match(line)
        if m:
            symbols.append({
                "name": m.group("name"),
                "kind": "function",
                "line": i,
                "end_line": None,
                "exported": bool(m.group("exp")),
                "async": bool(m.group("async")),
                "parent": None,
            })
            continue

        m = _CLASS.match(line)
        if m:
            current_class = m.group("name")
            class_indent = indent
            symbols.append({
                "name": m.group("name"),
                "kind": "class",
                "line": i,
                "end_line": None,
                "exported": bool(m.group("exp")),
                "async": False,
                "parent": None,
            })
            continue

        m = _IFACE.match(line)
        if m:
            symbols.append({
                "name": m.group("name"),
                "kind": "interface",
                "line": i,
                "end_line": None,
                "exported": bool(m.group("exp")),
                "async": False,
                "parent": None,
            })
            continue

        m = _TYPE.match(line)
        if m:
            symbols.append({
                "name": m.group("name"),
                "kind": "type",
                "line": i,
                "end_line": None,
                "exported": bool(m.group("exp")),
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
                "exported": bool(m.group("exp")),
                "async": False,
                "parent": None,
            })
            continue

        if indent <= 2:
            m = _CONST.match(line)
            if m and m.group("name") not in ("if", "else", "for", "while", "switch", "return"):
                symbols.append({
                    "name": m.group("name"),
                    "kind": "const",
                    "line": i,
                    "end_line": None,
                    "exported": bool(m.group("exp")),
                    "async": False,
                    "parent": None,
                })
                continue

        if current_class and indent > class_indent:
            m = _METHOD.match(line)
            if m and m.group("name") not in ("if", "else", "for", "while", "switch", "return", "catch", "constructor"):
                symbols.append({
                    "name": m.group("name"),
                    "kind": "function",
                    "line": i,
                    "end_line": None,
                    "exported": False,
                    "async": bool(m.group("async")),
                    "parent": current_class,
                })

    seen_sources = set()

    for m in _IMPORT_NAMES.finditer(content):
        src = m.group("src")
        names = [n.strip().split(" as ")[0].strip() for n in m.group("names").split(",") if n.strip()]
        if src not in seen_sources:
            imports.append({"source": src, "names": names})
            seen_sources.add(src)

    for m in _IMPORT_DEFAULT.finditer(content):
        src = m.group("src")
        if src not in seen_sources:
            imports.append({"source": src, "names": [m.group("name")]})
            seen_sources.add(src)

    for m in _IMPORT_STAR.finditer(content):
        src = m.group("src")
        if src not in seen_sources:
            imports.append({"source": src, "names": "*"})
            seen_sources.add(src)

    for m in _IMPORT_SIDE.finditer(content):
        src = m.group("src")
        if src not in seen_sources:
            imports.append({"source": src, "names": None})
            seen_sources.add(src)

    for m in _REQUIRE.finditer(content):
        src = m.group("src")
        if src not in seen_sources:
            imports.append({"source": src, "names": None})
            seen_sources.add(src)

    tests = _extract_tests_js(content, lines)

    return {
        "lang": "typescript" if path.endswith((".ts", ".tsx")) else "javascript",
        "lines": line_count,
        "symbols": symbols,
        "imports": imports,
        "tests": tests,
    }


def _extract_tests_js(content: str, lines: list[str]) -> dict | None:
    describes = list(_TEST_DESCRIBE.finditer(content))
    cases = list(_TEST_CASE.finditer(content))

    if not describes and not cases:
        return None

    def _line_num(match):
        return content[:match.start()].count("\n") + 1

    framework = "vitest"
    for line in lines[:20]:
        if "jest" in line:
            framework = "jest"
            break
        if "vitest" in line:
            framework = "vitest"
            break
        if "mocha" in line or "chai" in line:
            framework = "mocha"
            break

    suites = []
    if describes:
        for d in describes:
            suite = {"name": d.group("name"), "line": _line_num(d), "cases": []}
            suites.append(suite)
        for c in cases:
            c_line = _line_num(c)
            best_suite = None
            for s in suites:
                if s["line"] <= c_line:
                    best_suite = s
            if best_suite:
                best_suite["cases"].append({"name": c.group("name"), "line": c_line})
            else:
                suites.append({"name": "(root)", "line": c_line, "cases": [{"name": c.group("name"), "line": c_line}]})
    else:
        root_cases = [{"name": c.group("name"), "line": _line_num(c)} for c in cases]
        suites.append({"name": "(root)", "line": root_cases[0]["line"], "cases": root_cases})

    return {"framework": framework, "suites": suites}
