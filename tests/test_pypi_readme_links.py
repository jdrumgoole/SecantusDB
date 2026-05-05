"""Validate every link / image in the README served on PyPI resolves.

PyPI doesn't know our git repo, so any relative URL in `README.md`
(`docs/foo.md`, `brandkit/logo.svg`, `LICENSE`) renders broken on the
project page's long_description. This test fetches the published
markdown from PyPI's JSON API, extracts every link/image URL, and HEAD-
requests each to confirm a 2xx. A regression here means the next user
who reads the PyPI page sees broken links.

Networked + depends on a published release: skipped under the default
parallel suite via the ``online`` marker. Run with::

    uv run python -m pytest -p no:xdist -o addopts= -m online tests/test_pypi_readme_links.py

or via ``invoke validate-readme`` (same logic, surfaces failures with a
ranked report).
"""

from __future__ import annotations

import json
import re
import urllib.error
import urllib.request
from collections.abc import Iterator

import pytest

pytestmark = pytest.mark.online


_PYPI_JSON = "https://pypi.org/pypi/SecantusDB/json"
_USER_AGENT = "secantus-readme-link-test/1.0 (+https://github.com/jdrumgoole/SecantusDB)"
# Fragment-only, mailto:, tel:, and javascript: URLs aren't fetched.
_SKIP_SCHEMES = ("mailto:", "tel:", "javascript:")


def _fetch_pypi_readme() -> str:
    req = urllib.request.Request(_PYPI_JSON, headers={"User-Agent": _USER_AGENT})
    with urllib.request.urlopen(req, timeout=15) as resp:
        return json.load(resp)["info"].get("description") or ""


def _extract_urls(markdown: str) -> Iterator[str]:
    """Pull every URL out of the markdown source.

    Covers:
      - markdown ``[text](url)`` links (with optional ``"title"`` segment)
      - HTML ``<a href="url">`` and ``<img src="url">``
      - ``<source srcset="url">`` (the dark-mode logo's <picture> source)
      - bare ``<https://...>`` autolinks
    """
    # Markdown link/image: [text](url) or ![alt](url) — disallow whitespace
    # in the URL slot so a stray paragraph break doesn't gobble half a doc.
    for m in re.finditer(r"!?\[[^\]]*\]\((\S+?)(?:\s+\"[^\"]*\")?\)", markdown):
        yield m.group(1)
    # HTML attributes
    for m in re.finditer(r"<(?:a|img|source)\b[^>]*?\s(?:href|src|srcset)=\"([^\"]+)\"", markdown):
        yield m.group(1)
    # Bare autolinks
    for m in re.finditer(r"<(https?://[^>\s]+)>", markdown):
        yield m.group(1)


def _should_check(url: str) -> bool:
    if url.startswith("#"):  # in-page anchor
        return False
    if url.startswith(_SKIP_SCHEMES):
        return False
    return url.startswith(("http://", "https://"))


def _is_reachable(url: str) -> tuple[bool, str]:
    """Try HEAD; fall back to GET (some servers reject HEAD).

    Returns ``(ok, detail)``. ``ok`` is True for any 2xx or 3xx; the
    detail string is the HTTP status / exception class for diagnostics.
    """
    for method in ("HEAD", "GET"):
        req = urllib.request.Request(url, method=method, headers={"User-Agent": _USER_AGENT})
        try:
            with urllib.request.urlopen(req, timeout=20) as resp:
                if 200 <= resp.status < 400:
                    return True, f"{method} {resp.status}"
                if method == "GET":
                    return False, f"{method} {resp.status}"
        except urllib.error.HTTPError as e:
            # Some servers return 405 on HEAD but allow GET; only fail
            # after both methods give up.
            if method == "HEAD" and e.code in (405, 403):
                continue
            if method == "GET" and 200 <= e.code < 400:
                return True, f"GET {e.code}"
            return False, f"{method} HTTP {e.code}"
        except urllib.error.URLError as e:
            return False, f"{method} URLError: {e.reason}"
        except Exception as e:  # pragma: no cover — surfaces unexpected
            return False, f"{method} {type(e).__name__}: {e}"
    return False, "exhausted methods"


def test_pypi_readme_links_resolve() -> None:
    """Every URL in the rendered PyPI README must return a 2xx/3xx.

    Failures point at relative paths that escaped the absolute-URL
    sweep (PyPI can't resolve them) or genuinely-dead targets that
    moved or were deleted. Either is a real regression for users
    landing on the PyPI project page.
    """
    md = _fetch_pypi_readme()
    if not md:
        pytest.skip("PyPI returned an empty description (package not published yet?)")
    urls = sorted({u for u in _extract_urls(md) if _should_check(u)})
    assert urls, "no checkable URLs found in PyPI README — extractor regression?"
    failures: list[tuple[str, str]] = []
    for url in urls:
        ok, detail = _is_reachable(url)
        if not ok:
            failures.append((url, detail))
    if failures:
        report = "\n".join(f"  {url}\n    {detail}" for url, detail in failures)
        pytest.fail(f"{len(failures)}/{len(urls)} URLs unreachable in PyPI README:\n{report}")
