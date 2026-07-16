from __future__ import annotations

import re
from pathlib import Path

_REPO_ROOT = Path(__file__).resolve().parent.parent

# Single-source the rendered version from the lockstep crate version —
# the Rust server versions independently of the PyPI package
# (CLAUDE.md "the two servers version independently"), and the
# canonical carrier is crates/secantusdb/Cargo.toml. A plain regex
# read keeps conf.py dependency-free.
project = "SecantusDB Rust server"
author = "Joe Drumgoole"
copyright = "2026, Joe Drumgoole"
_cargo = (_REPO_ROOT / "crates" / "secantusdb" / "Cargo.toml").read_text(encoding="utf-8")
_m = re.search(r'^version\s*=\s*"([^"]+)"', _cargo, re.MULTILINE)
release = _m.group(1) if _m else "unknown"
version = release

extensions = [
    "myst_parser",
]

source_suffix = {
    ".md": "markdown",
}

templates_path = ["_templates"]
exclude_patterns = [
    "_build",
    "Thumbs.db",
    ".DS_Store",
]

myst_enable_extensions = [
    "colon_fence",
    "deflist",
]
myst_heading_anchors = 4

html_theme = "furo"
html_title = f"SecantusDB Rust DB {release}"
# The standard site banner, so the docs read as part of secantusdb.com.
html_theme_options = {
    "announcement": (
        '<a href="https://secantusdb.com/"><strong>SecantusDB</strong></a> &nbsp;·&nbsp; '
                '<a href="https://secantusdb.com/python-db.html">Python DB</a> &nbsp;·&nbsp; '
                '<a href="https://secantusdb.com/rust-db.html">Rust DB</a> &nbsp;·&nbsp; '
                '<a href="https://secantusdb.com/blog.html">Blog</a> &nbsp;·&nbsp; '
                '<a href="https://secantusdb.com/docs/index.html">Python docs</a> &nbsp;·&nbsp; '
                '<a href="https://secantusdb.com/docs/rust/index.html">Rust docs</a>'
    ),
}
html_static_path: list[str] = []
