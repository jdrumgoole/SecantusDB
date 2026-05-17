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
# Brand assets — see brandkit/ at the repo root for the full kit and
# brandkit/brand.html for the guided tour. The light/dark wordmark
# pair gives furo a logo per colour scheme; the favicon is one inline
# SVG.
#
# Don't set `html_logo` alongside Furo's `light_logo`/`dark_logo` —
# Furo renders BOTH and the sidebar shows two stacked wordmarks.
# We're locked to Furo, so the theme-specific options are enough.
html_favicon = "_static/favicon.svg"
html_theme_options = {
    "light_logo": "wordmark-horizontal.svg",
    "dark_logo": "wordmark-horizontal-on-dark.svg",
    # Brand palette: slate (neutral) + cyan (accent), matching
    # brandkit/README.md. Tokens map onto Tailwind's slate-* /
    # cyan-* scales.
    "light_css_variables": {
        "color-brand-primary": "#0e7490",        # cyan-700, AA on white
        "color-brand-content": "#0891b2",        # cyan-600
        "color-foreground-primary": "#0f172a",   # slate-900
        "color-foreground-secondary": "#475569", # slate-600
    },
    "dark_css_variables": {
        "color-brand-primary": "#22d3ee",        # cyan-400, AA on slate-900
        "color-brand-content": "#67e8f9",        # cyan-300
        "color-background-primary": "#0f172a",   # slate-900
        "color-background-secondary": "#1e293b", # slate-800
    },
}

intersphinx_mapping = {
    "python": ("https://docs.python.org/3", None),
    "pymongo": ("https://pymongo.readthedocs.io/en/stable/", None),
}

autodoc_member_order = "bysource"
myst_enable_extensions = ["colon_fence", "deflist"]
# Auto-generate slug-shaped anchors for ## / ### / #### headings so
# cross-doc links like ``[Querying the oplog](change-streams.md#
# querying-the-oplog-directly)`` resolve under ``-W``.
myst_heading_anchors = 4
