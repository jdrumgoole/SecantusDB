"""Parser for ``docs/changelog.md``.

The changelog is the system of record for what shipped in each release.
Format (rough Keep-a-Changelog with a prose-lede extension):

    ## [VERSION] — YYYY-MM-DD            <- release header

    ### Short headline title              <- becomes the blog post title

    First prose paragraph (lede)...       <- becomes the blog post body
    Second prose paragraph...
    Third prose paragraph...

    #### Added                            <- bullet detail (stays in
    - Foo                                    changelog, NOT lifted into
    - Bar                                    blog post body)

    #### Changed
    - ...

    ## [NEXT VERSION] ...

The parser splits the file into release entries, extracts version /
date / title / prose lede / structured sections. Used by
``changelog.blog`` to generate the matching blog post and by tests to
verify the changelog stays well-formed.

``Unreleased`` is recognised as a sentinel version name; it has no
date and never generates a blog post.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from pathlib import Path

_RELEASE_HEADER_RE = re.compile(
    r"^##\s+\[(?P<version>[^\]]+)\](?:\s+[—–-]\s+(?P<date>\d{4}-\d{2}-\d{2}))?\s*$"
)
_TITLE_RE = re.compile(r"^###\s+(?P<title>.+?)\s*$")
_SUBSECTION_RE = re.compile(r"^####\s+(?P<name>.+?)\s*$")


@dataclass
class Release:
    """One release entry from the changelog."""

    version: str  # ``"0.5.1b16"`` or ``"Unreleased"``
    date: str | None  # ``"2026-05-16"`` or ``None`` for Unreleased
    title: str  # the ``### ...`` headline
    lede: str  # prose paragraphs between title and the first ``####``
    sections: dict[str, list[str]] = field(default_factory=dict)
    # ``sections[name]`` is the list of bullet lines (with leading
    # ``- ``) verbatim. Order of insertion preserved.

    @property
    def is_unreleased(self) -> bool:
        return self.version.lower() == "unreleased"

    @property
    def tag(self) -> str:
        """The git tag form. ``"0.5.1b16"`` → ``"v0.5.1b16"``."""
        return f"v{self.version}"


def parse_changelog(path: Path) -> list[Release]:
    """Parse the changelog file and return its release entries in
    file order (newest-first by convention).
    """
    text = path.read_text(encoding="utf-8")
    return parse_text(text)


def parse_text(text: str) -> list[Release]:
    """Parse the changelog markdown content into release entries."""
    lines = text.splitlines()
    releases: list[Release] = []
    current: Release | None = None
    current_section: str | None = None
    lede_lines: list[str] = []
    seen_title = False

    for raw in lines:
        line = raw.rstrip()
        m_header = _RELEASE_HEADER_RE.match(line)
        if m_header:
            _flush_lede(current, lede_lines, seen_title)
            lede_lines = []
            seen_title = False
            current_section = None
            current = Release(
                version=m_header.group("version"),
                date=m_header.group("date"),
                title="",
                lede="",
            )
            releases.append(current)
            continue
        if current is None:
            # Pre-amble (file-level header, intro paragraphs) — skip.
            continue
        m_title = _TITLE_RE.match(line)
        if m_title and not seen_title:
            current.title = m_title.group("title")
            seen_title = True
            continue
        m_sub = _SUBSECTION_RE.match(line)
        if m_sub:
            _flush_lede(current, lede_lines, seen_title)
            lede_lines = []
            current_section = m_sub.group("name")
            current.sections.setdefault(current_section, [])
            continue
        if current_section is not None:
            # Bullet lines inside an Added / Changed / Fixed / etc. section.
            if line.startswith("- ") or line.startswith("  "):
                current.sections[current_section].append(line)
            elif not line.strip():
                # Blank line — keep section open for the next bullet.
                continue
            else:
                # Non-bullet text inside a section — uncommon, treat as
                # continuation of the previous bullet.
                if current.sections[current_section]:
                    current.sections[current_section][-1] += " " + line
        else:
            # Inside the release header / title / lede block — collect
            # for the prose lede. The opening blank lines before the
            # ``### title`` are skipped because seen_title is False
            # then; we only start collecting once the title has been
            # seen.
            if seen_title:
                lede_lines.append(raw)

    _flush_lede(current, lede_lines, seen_title)
    return releases


def _flush_lede(release: Release | None, lede_lines: list[str], seen_title: bool) -> None:
    if release is None or not seen_title or not lede_lines:
        return
    # Trim leading and trailing blank lines from the prose lede; collapse
    # interior blank-line runs into single blank lines.
    cleaned = "\n".join(lede_lines).strip()
    if not cleaned:
        return
    cleaned = re.sub(r"\n{3,}", "\n\n", cleaned)
    # Only ever overwrite a previously-set lede with new content — the
    # parser calls flush at every ``####`` subsection AND at end-of-file,
    # and the trailing flush would otherwise wipe the lede with "".
    release.lede = cleaned


def find_release(releases: list[Release], version: str) -> Release | None:
    """Return the release entry for ``version`` (string match, accepts
    ``"0.5.1b16"`` or ``"v0.5.1b16"``). ``None`` if not present.
    """
    target = version.lstrip("v")
    for r in releases:
        if r.version == target:
            return r
    return None
