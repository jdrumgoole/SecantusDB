"""Pelican configuration for the secantusdb.com marketing site (development).

Production overrides live in ``publishconf.py``. Run a local preview with
``cd website && uv run python -m invoke serve`` (see ``website/tasks.py``).
"""

from __future__ import annotations

import sys
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parent

sys.path.insert(0, str(HERE))


def _read_project_version() -> str:
    pyproject = tomllib.loads((REPO_ROOT / "pyproject.toml").read_text())
    return pyproject["project"]["version"]


SECANTUS_VERSION: str = _read_project_version()
SECANTUS_IS_ALPHA: bool = "a" in SECANTUS_VERSION.split(".")[-1]
SECANTUS_DOCS_URL: str = "https://secantusdb.readthedocs.io/en/latest/"
SECANTUS_PYPI_URL: str = "https://pypi.org/project/SecantusDB/"
SECANTUS_GITHUB_URL: str = "https://github.com/jdrumgoole/SecantusDB"
SECANTUS_TAGLINE: str = "THE SQLITE OF DOCUMENT DATABASES"

AUTHOR = "Joe Drumgoole"
SITENAME = "SecantusDB"
SITESUBTITLE = "The SQLite of document databases"
SITEURL = ""

PATH = "content"
THEME = str(HERE / "themes" / "secantus")
TIMEZONE = "Europe/Dublin"
DEFAULT_LANG = "en"

ARTICLE_PATHS = ["blog"]
ARTICLE_URL = "blog/{slug}.html"
ARTICLE_SAVE_AS = "blog/{slug}.html"
PAGE_PATHS = ["pages"]
PAGE_URL = "{slug}.html"
PAGE_SAVE_AS = "{slug}.html"

# `.html` URLs end-to-end so S3 + CloudFront serve every page directly
# from a real key — no CloudFront Function or Lambda@Edge needed for
# directory-style index resolution.
INDEX_SAVE_AS = "blog.html"
INDEX_URL = "blog.html"
ARCHIVES_SAVE_AS = "archives.html"
ARCHIVES_URL = "archives.html"
TAGS_SAVE_AS = "tags.html"
TAG_URL = "tag/{slug}.html"
TAG_SAVE_AS = "tag/{slug}.html"
CATEGORIES_SAVE_AS = "categories.html"
CATEGORY_URL = "category/{slug}.html"
CATEGORY_SAVE_AS = "category/{slug}.html"
AUTHORS_SAVE_AS = "authors.html"
AUTHOR_URL = "author/{slug}.html"
AUTHOR_SAVE_AS = "author/{slug}.html"
DIRECT_TEMPLATES = ["index", "archives"]

DEFAULT_PAGINATION = 10
DEFAULT_DATE_FORMAT = "%-d %B %Y"

FEED_ALL_ATOM = None
CATEGORY_FEED_ATOM = None
TRANSLATION_FEED_ATOM = None
AUTHOR_FEED_ATOM = None
AUTHOR_FEED_RSS = None

MARKDOWN = {
    "extension_configs": {
        "markdown.extensions.codehilite": {"css_class": "highlight"},
        "markdown.extensions.extra": {},
        "markdown.extensions.meta": {},
        "markdown.extensions.toc": {},
    },
    "output_format": "html5",
}

STATIC_PATHS: list[str] = []
ARTICLE_EXCLUDES: list[str] = ["pages"]

PLUGIN_PATHS = [str(HERE / "plugins")]
PLUGINS: list[str] = []

JINJA_GLOBALS = {
    "SECANTUS_VERSION": SECANTUS_VERSION,
    "SECANTUS_IS_ALPHA": SECANTUS_IS_ALPHA,
    "SECANTUS_DOCS_URL": SECANTUS_DOCS_URL,
    "SECANTUS_PYPI_URL": SECANTUS_PYPI_URL,
    "SECANTUS_GITHUB_URL": SECANTUS_GITHUB_URL,
    "SECANTUS_TAGLINE": SECANTUS_TAGLINE,
}


def _pyhighlight(code: str) -> str:
    """Render Python source as HTML with the same Pygments classes that
    markdown's codehilite extension produces, so the side-by-side
    comparison on the home page reads as one consistent code block."""
    from pygments import highlight
    from pygments.formatters import HtmlFormatter
    from pygments.lexers import PythonLexer
    return highlight(code.strip("\n"), PythonLexer(), HtmlFormatter(cssclass="highlight"))


JINJA_FILTERS = {"pyhighlight": _pyhighlight}

RELATIVE_URLS = True
