"""Changelog fragments — conflict-free, per-PR changelog entries.

The problem this solves: when several sessions each open a PR that edits the top
of ``docs/changelog.md``'s ``## [Unreleased]`` section, every pair of concurrent
PRs conflicts on that one file — a merge to ``main`` forces the other branch to
rebase and re-resolve the changelog by hand. The same is true of the single
``version`` line in ``pyproject.toml`` when every PR bumps it.

The fix: a PR **does not touch ``docs/changelog.md``**. Instead it adds a new file
``changelog.d/<slug>.md`` holding exactly one changelog entry — a ``### Headline``
line, a prose lede, then the ``#### Added`` / ``#### Fixed`` / … sections. New
files never conflict with each other, so concurrent PRs are independent.

At release time :func:`collate` folds every fragment into the ``## [Unreleased]``
section of ``docs/changelog.md`` (in filename order) and deletes the fragment
files, leaving the changelog in exactly the shape it would have had if the entries
had been written inline. The regular promote-``[Unreleased]``-to-a-dated-section
release step then proceeds unchanged.
"""

from __future__ import annotations

from pathlib import Path

FRAGMENTS_DIR = Path("changelog.d")
CHANGELOG = Path("docs/changelog.md")
UNRELEASED_HEADER = "## [Unreleased]"

#: Fragment files that are documentation/placeholder, not changelog entries.
_NON_FRAGMENT_NAMES = frozenset({"readme.md", ".gitkeep"})


def fragment_files(fragments_dir: Path = FRAGMENTS_DIR) -> list[Path]:
    """Every changelog-fragment file, sorted by name.

    Excludes the ``README.md`` / ``.gitkeep`` scaffolding. Returns ``[]`` when the
    directory is absent so callers can treat "no fragments" uniformly.
    """
    if not fragments_dir.exists():
        return []
    return sorted(
        p
        for p in fragments_dir.iterdir()
        if p.is_file() and p.name.lower() not in _NON_FRAGMENT_NAMES
    )


def collate(
    changelog_path: Path = CHANGELOG,
    fragments_dir: Path = FRAGMENTS_DIR,
    *,
    delete: bool = True,
) -> list[Path]:
    """Fold all fragments into the ``[Unreleased]`` section of ``changelog_path``.

    Fragment bodies are inserted directly under the ``## [Unreleased]`` header, in
    filename order, each separated by a blank line. The fragment files are removed
    (unless ``delete=False``). Returns the list of fragment paths that were folded
    (``[]`` when there were none).

    Raises ``ValueError`` if the changelog has no ``## [Unreleased]`` header.
    """
    frags = fragment_files(fragments_dir)
    if not frags:
        return []

    bodies = [body for p in frags if (body := p.read_text().strip())]

    text = changelog_path.read_text()
    if UNRELEASED_HEADER not in text:
        raise ValueError(f"{changelog_path} has no '{UNRELEASED_HEADER}' section")

    # Insert right after the header line (and its trailing newline, if present),
    # so the newest fragments land at the top of [Unreleased].
    anchor = text.index(UNRELEASED_HEADER) + len(UNRELEASED_HEADER)
    insertion = "\n\n" + "\n\n".join(bodies)
    new_text = text[:anchor] + insertion + text[anchor:]

    changelog_path.write_text(new_text)
    if delete:
        for p in frags:
            p.unlink()
    return frags
