"""Core indexer: walks a repo, extracts symbols, builds the index JSON."""

import json
import os
import time
from collections import defaultdict
from pathlib import Path

from .extractors import EXTRACTORS, LANGUAGE_MAP
from .schema import SCHEMA_VERSION

DEFAULT_IGNORE_DIRS = {
    "node_modules", ".git", "__pycache__", ".venv", "venv", "env",
    "dist", "build", ".next", ".nuxt", "out", "target",
    ".cache", ".tox", ".mypy_cache", ".ruff_cache", ".pytest_cache",
    "coverage", ".nyc_output", ".turbo", ".svelte-kit",
    "vendor", "deps", "_build", ".elixir_ls",
    "graphify-out", ".drift",
}

DEFAULT_IGNORE_FILES = {
    "package-lock.json", "yarn.lock", "pnpm-lock.yaml",
    "Cargo.lock", "poetry.lock", "Pipfile.lock",
    ".DS_Store", "Thumbs.db",
}

MAX_FILE_SIZE = 512 * 1024


def index_repo(
    root: str | Path,
    *,
    extra_ignore_dirs: set[str] | None = None,
    extra_ignore_files: set[str] | None = None,
    respect_gitignore: bool = True,
) -> dict:
    root = Path(root).resolve()
    ignore_dirs = DEFAULT_IGNORE_DIRS | (extra_ignore_dirs or set())
    ignore_files = DEFAULT_IGNORE_FILES | (extra_ignore_files or set())

    gitignore_patterns = []
    if respect_gitignore:
        gitignore_patterns = _load_gitignore(root)

    files_data = {}
    tests_data = {}
    symbol_index: dict[str, list] = defaultdict(list)
    languages_seen = set()

    for filepath in _walk_files(root, ignore_dirs, ignore_files, gitignore_patterns):
        ext = filepath.suffix.lower()
        extractor = EXTRACTORS.get(ext)
        if not extractor:
            continue

        rel_path = filepath.relative_to(root).as_posix()

        try:
            if filepath.stat().st_size > MAX_FILE_SIZE:
                continue
            content = filepath.read_text(encoding="utf-8", errors="replace")
        except (OSError, UnicodeDecodeError):
            continue

        result = extractor(content, rel_path)
        lang = LANGUAGE_MAP.get(ext, ext.lstrip("."))
        languages_seen.add(lang)

        file_entry = {
            "lang": result["lang"],
            "lines": result["lines"],
            "symbols": result["symbols"],
            "imports": result["imports"],
        }
        files_data[rel_path] = file_entry

        for sym in result["symbols"]:
            symbol_index[sym["name"]].append({
                "file": rel_path,
                "line": sym["line"],
                "kind": sym["kind"],
                "exported": sym.get("exported", False),
            })

        if result.get("tests"):
            tests_data[rel_path] = result["tests"]

    return {
        "version": SCHEMA_VERSION,
        "repo": root.name,
        "root": str(root),
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "languages": sorted(languages_seen),
        "files": dict(sorted(files_data.items())),
        "tests": dict(sorted(tests_data.items())),
        "symbol_index": dict(sorted(symbol_index.items())),
    }


def compact_index(index: dict) -> dict:
    """Strip null/false/empty fields to reduce size."""
    compact = {**index}
    compact["files"] = {}
    for path, fdata in index["files"].items():
        symbols = []
        for sym in fdata["symbols"]:
            s = {"name": sym["name"], "kind": sym["kind"], "line": sym["line"]}
            if sym.get("exported"):
                s["exported"] = True
            if sym.get("async"):
                s["async"] = True
            if sym.get("parent"):
                s["parent"] = sym["parent"]
            if sym.get("decorators"):
                s["decorators"] = sym["decorators"]
            symbols.append(s)
        entry = {"lang": fdata["lang"], "lines": fdata["lines"], "symbols": symbols}
        if fdata["imports"]:
            entry["imports"] = fdata["imports"]
        compact["files"][path] = entry

    compact["symbol_index"] = {}
    for name, locs in index["symbol_index"].items():
        exported_locs = [{"file": l["file"], "line": l["line"], "kind": l["kind"]} for l in locs if l.get("exported")]
        if exported_locs:
            compact["symbol_index"][name] = exported_locs

    return compact


def write_index(index: dict, output_path: str | Path, *, compact: bool = False) -> Path:
    output_path = Path(output_path)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    data = compact_index(index) if compact else index
    output_path.write_text(
        json.dumps(data, separators=(",", ":") if compact else None, indent=None if compact else 2, ensure_ascii=False),
        encoding="utf-8",
    )
    return output_path


def _walk_files(
    root: Path,
    ignore_dirs: set[str],
    ignore_files: set[str],
    gitignore_patterns: list[str],
) -> list[Path]:
    result = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [
            d for d in dirnames
            if d not in ignore_dirs
            and not d.startswith(".")
            and not _matches_gitignore(Path(dirpath, d).relative_to(root).as_posix() + "/", gitignore_patterns)
        ]
        dirnames.sort()

        for fname in sorted(filenames):
            if fname in ignore_files:
                continue
            fpath = Path(dirpath, fname)
            rel = fpath.relative_to(root).as_posix()
            if not _matches_gitignore(rel, gitignore_patterns):
                result.append(fpath)

    return result


def _load_gitignore(root: Path) -> list[str]:
    gitignore = root / ".gitignore"
    if not gitignore.exists():
        return []
    patterns = []
    for line in gitignore.read_text(errors="replace").splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            patterns.append(line)
    return patterns


def _matches_gitignore(rel_path: str, patterns: list[str]) -> bool:
    for pattern in patterns:
        clean = pattern.rstrip("/")
        if clean in rel_path or rel_path.startswith(clean + "/"):
            return True
        if clean.startswith("*.") and rel_path.endswith(clean[1:]):
            return True
    return False
