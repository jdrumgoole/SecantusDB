from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))

project = "SecantusDB"
author = "Joe Drumgoole"
copyright = "2026, Joe Drumgoole"
release = "0.2.0a9"

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
