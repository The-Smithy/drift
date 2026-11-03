"""Extract symbols and imports from Python files."""

import re

_FUNC = re.compile(
    r"^(?P<indent>\s*)(?P<async>async\s+)?def\s+(?P<name>\w+)\s*\(",
    re.MULTILINE,
)

_CLASS = re.compile(
    r"^(?P<indent>\s*)class\s+(?P<name>\w+)",
    re.MULTILINE,
)

_CONST = re.compile(
    r"^(?P<name>[A-Z][A-Z_0-9]+)\s*(?::\s*\w[^=]*)?\s*=",
    re.MULTILINE,
)

_IMPORT_FROM = re.compile(
    r"^from\s+(?P<src>[.\w]+)\s+import\s+(?P<names>.+?)(?:\s*#.*)?$",
    re.MULTILINE,
)

_IMPORT = re.compile(
    r"^import\s+(?P<names>[.\w]+(?:\s*,\s*[.\w]+)*)",
    re.MULTILINE,
)

_DECORATOR = re.compile(r"^\s*@(\w+(?:\.\w+)*)")

_TEST_CLASS = re.compile(r"^class\s+(?P<name>Test\w+)", re.MULTILINE)
_TEST_FUNC = re.compile(r"^(?P<indent>\s*)def\s+(?P<name>test_\w+)", re.MULTILINE)
_PYTEST_MARK = re.compile(r"@pytest\.mark\.(?P<mark>\w+)")


def extract_python(content: str, path: str) -> dict:
    lines = content.split("\n")
    line_count = len(lines)
    symbols = []
    imports = []

    current_class = None
    class_indent = -1

    decorator_map: dict[int, list[str]] = {}
    for i, line in enumerate(lines):
        m = _DECORATOR.match(line)
        if m:
            target_line = i + 1
            while target_line < len(lines) and _DECORATOR.match(lines[target_line]):
                target_line += 1
            decorator_map.setdefault(target_line, []).append(m.group(1))

    for i, line in enumerate(lines, 1):
        stripped = line.lstrip()
        indent = len(line) - len(stripped)

        if current_class and indent <= class_indent and stripped and not stripped.startswith("#"):
            current_class = None
            class_indent = -1

        m = _CLASS.match(line)
        if m:
            ci = len(m.group("indent"))
            if ci == 0:
                current_class = m.group("name")
                class_indent = ci
            symbols.append({
                "name": m.group("name"),
                "kind": "class",
                "line": i,
                "end_line": None,
                "exported": not m.group("name").startswith("_"),
                "async": False,
                "parent": current_class if ci > 0 else None,
                "decorators": decorator_map.get(i - 1, []),
            })
            if ci == 0:
                current_class = m.group("name")
                class_indent = ci
            continue

        m = _FUNC.match(line)
        if m:
            fi = len(m.group("indent"))
            parent = current_class if (current_class and fi > class_indent) else None
            symbols.append({
                "name": m.group("name"),
                "kind": "function",
                "line": i,
                "end_line": None,
                "exported": not m.group("name").startswith("_"),
                "async": bool(m.group("async")),
                "parent": parent,
                "decorators": decorator_map.get(i - 1, []),
            })
            continue

        if indent == 0:
            m = _CONST.match(line)
            if m:
                symbols.append({
                    "name": m.group("name"),
                    "kind": "const",
                    "line": i,
                    "end_line": None,
                    "exported": True,
                    "async": False,
                    "parent": None,
                    "decorators": [],
                })

    for m in _IMPORT_FROM.finditer(content):
        src = m.group("src")
        raw_names = m.group("names")
        if raw_names.strip() == "(":
            continue
        names = [n.strip().split(" as ")[0].strip() for n in raw_names.split(",") if n.strip() and n.strip() != "("]
        if names == ["*"]:
            imports.append({"source": src, "names": "*"})
        else:
            imports.append({"source": src, "names": names})

    for m in _IMPORT.finditer(content):
        for mod in m.group("names").split(","):
            mod = mod.strip().split(" as ")[0].strip()
            if mod:
                imports.append({"source": mod, "names": None})

    tests = _extract_tests_python(content, lines, path)

    return {
        "lang": "python",
        "lines": line_count,
        "symbols": symbols,
        "imports": imports,
        "tests": tests,
    }


def _extract_tests_python(content: str, lines: list[str], path: str) -> dict | None:
    test_classes = list(_TEST_CLASS.finditer(content))
    test_funcs = list(_TEST_FUNC.finditer(content))

    if not test_classes and not test_funcs:
        return None

    def _line_num(match):
        return content[:match.start()].count("\n") + 1

    framework = "pytest"
    for line in lines[:20]:
        if "unittest" in line:
            framework = "unittest"
            break

    suites = []
    if test_classes:
        for tc in test_classes:
            tc_line = _line_num(tc)
            suite = {"name": tc.group("name"), "line": tc_line, "cases": []}
            suites.append(suite)

        for tf in test_funcs:
            tf_line = _line_num(tf)
            tf_indent = len(tf.group("indent"))
            if tf_indent > 0:
                best_suite = None
                for s in suites:
                    if s["line"] <= tf_line:
                        best_suite = s
                if best_suite:
                    best_suite["cases"].append({"name": tf.group("name"), "line": tf_line})
                    continue
            suites.append({"name": "(module)", "line": tf_line, "cases": [{"name": tf.group("name"), "line": tf_line}]})
    else:
        cases = [{"name": tf.group("name"), "line": _line_num(tf)} for tf in test_funcs]
        suites.append({"name": "(module)", "line": cases[0]["line"], "cases": cases})

    return {"framework": framework, "suites": suites}
