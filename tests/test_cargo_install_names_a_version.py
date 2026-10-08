"""Every published ``cargo install`` instruction names a ``--version``.

``secantus-mdb`` and ``secantus-pg`` each carry a ``0.0.0`` name-reservation
release on crates.io, and every real release so far is a pre-release. A bare
``cargo install secantus-mdb`` therefore resolves to ``0.0.0`` and stops with
"there is nothing to install ... because it has no binaries". The README, the
docs and the site all printed the bare command (measured 2026-10-08, cargo
1.98.1); with ``--version`` both install.

Drop this once each crate has a stable release newer than its placeholder.
"""

from __future__ import annotations

import pathlib
import re

_REPO = pathlib.Path(__file__).resolve().parent.parent

_BARE = re.compile(r"cargo install secantus-(?:mdb|pg)\b(?!\s+--version)")

#: What a user reads: the PyPI/GitHub README, both docs trees, the crates.io
#: READMEs and the site templates. The changelog and blog are history.
_PUBLISHED = (
    "README.md",
    "docs/*.md",
    "docs-rust/*.md",
    "crates/*/README.md",
    "website/themes/secantus/templates/**/*.html",
    "website/content/pages/*.md",
)
_HISTORY = {"docs/changelog.md"}


def _published_files() -> list[pathlib.Path]:
    files = {p for pattern in _PUBLISHED for p in _REPO.glob(pattern)}
    return sorted(p for p in files if p.relative_to(_REPO).as_posix() not in _HISTORY)


def test_scan_reaches_the_published_pages() -> None:
    names = {p.relative_to(_REPO).as_posix() for p in _published_files()}
    assert {"README.md", "docs/servers.md", "docs-rust/installation.md"} <= names


def test_no_bare_cargo_install() -> None:
    bare = [
        f"{path.relative_to(_REPO)}: {match.group(0)!r}"
        for path in _published_files()
        for match in _BARE.finditer(path.read_text(encoding="utf-8"))
    ]
    assert bare == [], "cargo install without --version installs nothing:\n" + "\n".join(bare)
