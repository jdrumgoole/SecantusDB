"""Tests for the changelog parser + blog generator.

The changelog is the system of record for what shipped in each
release. These tests guard the file shape (parser sees one entry per
release, with the fields the blog generator needs) and the
generator's output (round-trips back to a parseable Pelican post).
"""

from __future__ import annotations

from pathlib import Path

import pytest
from changelog.blog import blog_post_path, render_blog_post
from changelog.parse import (
    Release,
    find_release,
    parse_changelog,
    parse_text,
)

REPO_ROOT = Path(__file__).resolve().parent.parent
CHANGELOG_PATH = REPO_ROOT / "docs" / "changelog.md"


def test_changelog_file_parses_without_error() -> None:
    releases = parse_changelog(CHANGELOG_PATH)
    assert releases, "expected at least one release entry"


def test_every_released_entry_has_required_fields() -> None:
    for r in parse_changelog(CHANGELOG_PATH):
        if r.is_unreleased:
            continue
        assert r.date is not None, f"release {r.version} has no date"
        assert r.title, f"release {r.version} has no title"
        assert r.lede, f"release {r.version} has no prose lede"


def test_find_release_accepts_v_prefix() -> None:
    releases = parse_changelog(CHANGELOG_PATH)
    # Pull any known released version from the file rather than
    # hardcoding one that might disappear.
    sample = next(r for r in releases if not r.is_unreleased)
    assert find_release(releases, sample.version) is sample
    assert find_release(releases, f"v{sample.version}") is sample
    assert find_release(releases, "0.0.0-not-real") is None


def test_render_blog_post_round_trips_to_pelican_shape() -> None:
    releases = parse_changelog(CHANGELOG_PATH)
    sample = next(r for r in releases if not r.is_unreleased)
    post = render_blog_post(sample, time="13:45:00")
    # Frontmatter fields all present.
    assert post.startswith(f"Title: {sample.title}\n")
    assert f"Date: {sample.date} 13:45:00\n" in post
    expected_slug = f"release-{sample.version.replace('.', '-')}"
    assert f"Slug: {expected_slug}\n" in post
    assert "Author: Joe Drumgoole\n" in post
    assert "Category: Releases\n" in post
    assert "Tags: release\n" in post
    # Summary line under frontmatter.
    assert f"Summary: {sample.title} (v{sample.version}).\n" in post
    # Lede body present.
    assert sample.lede in post
    # Link bar at the bottom — all three links.
    assert f"releases/tag/v{sample.version}" in post
    assert f"pypi.org/project/SecantusDB/{sample.version}/" in post
    assert f"tree/v{sample.version}" in post


def test_render_blog_post_rejects_unreleased() -> None:
    [unreleased] = [r for r in parse_changelog(CHANGELOG_PATH) if r.is_unreleased]
    with pytest.raises(ValueError, match="Unreleased"):
        render_blog_post(unreleased)


def test_blog_post_path_follows_naming_convention(tmp_path: Path) -> None:
    r = Release(
        version="0.5.1b99",
        date="2026-06-01",
        title="Test",
        lede="Body.",
    )
    p = blog_post_path(r, tmp_path)
    assert p == tmp_path / "website" / "content" / "blog" / "2026-06-01-release-0-5-1b99.md"


# --- Parser-shape tests (synthetic input, not the real file) ---


def test_parser_extracts_sections_in_insertion_order() -> None:
    text = """\
# Changelog

Intro text.

## [1.2.3] — 2026-01-01

### Headline title

First paragraph of the lede.

Second paragraph.

#### Added
- Foo
- Bar

#### Changed
- Baz

#### Fixed
- Qux
"""
    [r] = parse_text(text)
    assert r.version == "1.2.3"
    assert r.date == "2026-01-01"
    assert r.title == "Headline title"
    assert r.lede == "First paragraph of the lede.\n\nSecond paragraph."
    assert list(r.sections.keys()) == ["Added", "Changed", "Fixed"]
    assert r.sections["Added"] == ["- Foo", "- Bar"]
    assert r.sections["Changed"] == ["- Baz"]
    assert r.sections["Fixed"] == ["- Qux"]


def test_parser_handles_unreleased_with_no_date() -> None:
    text = """\
# Changelog

## [Unreleased]

### Work in flight

Some prose.

#### Added
- New thing

## [0.1.0] — 2026-01-01

### First release

The first.
"""
    releases = parse_text(text)
    assert len(releases) == 2
    assert releases[0].is_unreleased
    assert releases[0].date is None
    assert releases[0].title == "Work in flight"
    assert releases[1].version == "0.1.0"
    assert not releases[1].is_unreleased
