from .typescript import extract_typescript
from .python import extract_python
from .rust import extract_rust
from .sql import extract_sql

EXTRACTORS = {
    ".ts": extract_typescript,
    ".tsx": extract_typescript,
    ".js": extract_typescript,
    ".jsx": extract_typescript,
    ".mjs": extract_typescript,
    ".cjs": extract_typescript,
    ".py": extract_python,
    ".rs": extract_rust,
    ".sql": extract_sql,
}

LANGUAGE_MAP = {
    ".ts": "typescript",
    ".tsx": "typescript",
    ".js": "javascript",
    ".jsx": "javascript",
    ".mjs": "javascript",
    ".cjs": "javascript",
    ".py": "python",
    ".rs": "rust",
    ".sql": "sql",
}
