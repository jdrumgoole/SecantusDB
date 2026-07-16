from __future__ import annotations

import pytest
from changelog.fragments import collate, fragment_files

_CHANGELOG = """# Changelog

## [Unreleased]

## [0.5.0] — 2026-01-01

### Old release

Body.
"""


def _write(dir_, name, text):
    p = dir_ / name
    p.write_text(text)
    return p


def test_fragment_files_excludes_scaffolding(tmp_path):
    d = tmp_path / "changelog.d"
    d.mkdir()
    _write(d, "README.md", "# readme")
    _write(d, ".gitkeep", "")
    a = _write(d, "aaa.md", "### A\n")
    b = _write(d, "bbb.md", "### B\n")
    assert fragment_files(d) == [a, b]  # sorted, scaffolding excluded


def test_fragment_files_missing_dir(tmp_path):
    assert fragment_files(tmp_path / "nope") == []


def test_collate_folds_in_filename_order(tmp_path):
    d = tmp_path / "changelog.d"
    d.mkdir()
    _write(d, "2-second.md", "### Second\n\nBody two.\n\n#### Fixed\n\n- two")
    _write(d, "1-first.md", "### First\n\nBody one.\n\n#### Added\n\n- one")
    cl = tmp_path / "changelog.md"
    cl.write_text(_CHANGELOG)

    folded = collate(cl, d)

    assert len(folded) == 2
    out = cl.read_text()
    # Both entries land under [Unreleased], first-by-filename on top.
    unreleased = out.split("## [0.5.0]")[0]
    assert "### First" in unreleased and "### Second" in unreleased
    assert unreleased.index("### First") < unreleased.index("### Second")
    # The old release section is untouched.
    assert "## [0.5.0] — 2026-01-01" in out
    # Fragment files are deleted.
    assert fragment_files(d) == []


def test_collate_no_fragments_is_noop(tmp_path):
    d = tmp_path / "changelog.d"
    d.mkdir()
    cl = tmp_path / "changelog.md"
    cl.write_text(_CHANGELOG)
    assert collate(cl, d) == []
    assert cl.read_text() == _CHANGELOG  # unchanged


def test_collate_keep_fragments(tmp_path):
    d = tmp_path / "changelog.d"
    d.mkdir()
    _write(d, "x.md", "### X\n\nBody.")
    cl = tmp_path / "changelog.md"
    cl.write_text(_CHANGELOG)
    collate(cl, d, delete=False)
    assert len(fragment_files(d)) == 1  # kept


def test_collate_requires_unreleased_header(tmp_path):
    d = tmp_path / "changelog.d"
    d.mkdir()
    _write(d, "x.md", "### X\n\nBody.")
    cl = tmp_path / "changelog.md"
    cl.write_text("# Changelog\n\n## [0.5.0] — 2026-01-01\n\nno unreleased\n")
    with pytest.raises(ValueError, match="Unreleased"):
        collate(cl, d)


def test_collate_skips_blank_fragments(tmp_path):
    d = tmp_path / "changelog.d"
    d.mkdir()
    _write(d, "empty.md", "   \n")
    _write(d, "real.md", "### Real\n\nBody.")
    cl = tmp_path / "changelog.md"
    cl.write_text(_CHANGELOG)
    collate(cl, d)
    out = cl.read_text()
    assert "### Real" in out
