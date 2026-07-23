"""Tests for gauge validation-report parsing.

The reports come in three shapes and the parser must handle all of them —
a positional parser tuned to one silently mis-reads the others.
"""

from __future__ import annotations

from pathlib import Path

from secantus.opsboard import reports

# Shape 1: per-category table with an **Overall** row (most gauges).
_PLAIN = """\
# mongo-go-driver Validation Report

Generated 2026-07-20 — SecantusDB 0.6.0b0 vs mongo-go-driver abc123.

| Package | Passed | Failed | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `mongo` | 300 | 0 | 40 | 340 | 100.0% |
| **Overall** | **401** | **0** | **52** | **453** | **100.0%** |
"""

# Shape 2: same, plus an extra Errored column (pymongo).
_WITH_ERRORED = """\
# pymongo Validation Report

Generated 2026-07-23 — SecantusDB 0.6.0b0 vs pymongo f2103a9.

| Category | Passed | Failed | Errored | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|---:|
| `test_cursor.py` | 64 | 3 | 0 | 5 | 72 | 95.5% |
| **Overall** | **1020** | **5** | **0** | **475** | **1500** | **99.5%** |
"""

# Shape 3: label-less single-row summary, header starts at Passed (C++ gauge).
_SUMMARY_ONLY = """\
# mongo-cxx-driver Validation Report

Generated 2026-07-19 — SecantusDB 0.6.0b0 vs mongo-cxx-driver 24852b6.

## Summary

| Passed | Failed | Skipped | Total | Pass rate |
|---:|---:|---:|---:|---:|
| 890 | 0 | 9 | 899 | 100.0% |
"""


def test_parses_overall_row() -> None:
    r = reports.parse(_PLAIN)
    assert r is not None
    assert (r.passed, r.failed, r.skipped, r.total) == (401, 0, 52, 453)
    assert r.rate == 100.0
    assert r.errored == 0
    assert r.generated == "2026-07-20"
    assert r.ran == 401  # total - skipped
    assert r.clean is True


def test_parses_errored_column_by_name_not_position() -> None:
    # The extra Errored column shifts every later field; a positional parser
    # would read Skipped as Errored here.
    r = reports.parse(_WITH_ERRORED)
    assert r is not None
    assert r.passed == 1020
    assert r.failed == 5
    assert r.errored == 0
    assert r.skipped == 475  # NOT 0 — the column after Errored
    assert r.total == 1500
    assert r.rate == 99.5
    assert r.clean is False  # 5 failures


def test_parses_label_less_summary_table() -> None:
    r = reports.parse(_SUMMARY_ONLY)
    assert r is not None
    assert (r.passed, r.failed, r.skipped, r.total) == (890, 0, 9, 899)
    assert r.summary == "890/890 · 100.0%"


def test_no_table_returns_none() -> None:
    assert reports.parse("# Report\n\nNothing here.\n") is None


def test_malformed_counts_return_none() -> None:
    bad = "| Passed | Total |\n|---|---|\n| **Overall** | n/a | n/a |\n"
    assert reports.parse(bad) is None


def test_dirty_report_flagged() -> None:
    r = reports.parse(_PLAIN.replace("**0**", "**7**", 1))
    assert r is not None
    assert r.failed == 7
    assert r.clean is False


# --------------------------------------------------------------------------- #
# Filename mapping
# --------------------------------------------------------------------------- #


def test_report_filenames() -> None:
    # pymongo is the unsuffixed base report; others carry their key.
    assert reports.report_filename("pymongo", "python") == "validation-report.md"
    assert reports.report_filename("pymongo", "rust") == "validation-report-rust-server.md"
    assert reports.report_filename("go", "python") == "validation-report-go.md"
    assert reports.report_filename("go", "rust") == "validation-report-go-rust-server.md"
    # The rust *driver* gauge vs the rust *server* suffix must not collide.
    assert reports.report_filename("rust", "python") == "validation-report-rust.md"
    assert reports.report_filename("rust", "rust") == "validation-report-rust-rust-server.md"


def test_load_missing_report_is_none(tmp_path: Path) -> None:
    assert reports.load(tmp_path, "go", "python") is None


def test_load_reads_from_docs(tmp_path: Path) -> None:
    docs = tmp_path / "docs"
    docs.mkdir()
    (docs / "validation-report-go.md").write_text(_PLAIN, encoding="utf-8")
    r = reports.load(tmp_path, "go", "python")
    assert r is not None
    assert r.passed == 401
    assert r.path == "validation-report-go.md"
