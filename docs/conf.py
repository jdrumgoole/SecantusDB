from __future__ import annotations

import sys
import tomllib
from pathlib import Path

_REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(_REPO_ROOT / "src"))

# Single-source the rendered version from pyproject.toml so the
# theme's sidebar title and any `|release|` references in the .md
# stay in sync with the package the docs are being built for.
# Reading from pyproject.toml (not `from secantus import __version__`)
# keeps conf.py side-effect-free even if WiredTiger fails to build —
# Sphinx will still render with a real version string.
project = "SecantusDB"
author = "Joe Drumgoole"
copyright = "2026, Joe Drumgoole"
release = tomllib.loads((_REPO_ROOT / "pyproject.toml").read_text(encoding="utf-8"))[
    "project"
]["version"]
version = release

extensions = [
    "myst_parser",
    "sphinx.ext.autodoc",
    "sphinx.ext.napoleon",
    "sphinx.ext.viewcode",
    "sphinx.ext.intersphinx",
]

source_suffix = {
    ".md": "markdown",
    ".rst": "restructuredtext",
}

templates_path = ["_templates"]
exclude_patterns = ["_build", "Thumbs.db", ".DS_Store"]

html_theme = "furo"
html_static_path = ["_static"]

intersphinx_mapping = {
    "python": ("https://docs.python.org/3", None),
    "pymongo": ("https://pymongo.readthedocs.io/en/stable/", None),
}

autodoc_member_order = "bysource"
myst_enable_extensions = ["colon_fence", "deflist"]
