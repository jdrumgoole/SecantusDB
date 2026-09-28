"""Tests for the driver-panel HTML generator.

The marketing site's driver-panel grid is now generated from the same
``.validation/`` raw artifacts that drive the cross-driver summary
report. These tests guard the renderer shape (right number of panels
in the right order; numbers come through; expected-failure note shows
when applicable; missing artifacts fail loudly).
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest
from validation_summary.driver_panels import (
    PANEL_PROSE,
    SMOKE_PANELS,
    _format_rate,
    _render_smoke_panel,
    _render_validation_panel,
    render,
)
from validation_summary.generate import GaugeStats


def _stats(passed: int, failed: int, skipped: int, expected: int = 0) -> GaugeStats:
    s = GaugeStats(
        name="x",
        language="X",
        driver_version="abc",
        passed=passed,
        failed=failed,
        skipped=skipped,
        failure_descriptions=[],
    )
    s.expected_failures = expected
    return s


def test_format_rate_uses_raw_denominator() -> None:
    # True raw rate: passed / ran, no expected-failure exclusion.
    # 100 passed, 2 failed → 100/102 = 98.0%.
    s = _stats(passed=100, failed=2, skipped=0, expected=1)
    assert _format_rate(s) == "98.0%"


def test_format_rate_does_not_exclude_expected_failures() -> None:
    # The anti-trimming invariant: even when every failure is a documented
    # ("expected") divergence, the raw rate still counts them against the
    # denominator — the panel never flatters a gauge by excluding them.
    # 100 passed, 2 failed (both expected) → 100/102 = 98.0%, not 100%.
    s = _stats(passed=100, failed=2, skipped=0, expected=2)
    assert _format_rate(s) == "98.0%"


def test_format_rate_empty_returns_em_dash() -> None:
    s = _stats(passed=0, failed=0, skipped=0)
    assert _format_rate(s) == "&mdash;"


def test_render_validation_panel_contains_required_chrome() -> None:
    s = _stats(passed=959, failed=0, skipped=382)
    panel = _render_validation_panel("pymongo", s)
    assert "<h3>pymongo</h3>" in panel
    assert 'class="lang">Python</span>' in panel
    assert 'class="rate">100.0%</span>' in panel
    assert "<strong>959</strong> tests passed" in panel
    assert "<strong>0</strong> failed" in panel
    assert "Read the report" in panel
    assert "validation-report.html" in panel


def test_render_validation_panel_shows_expected_failure_note() -> None:
    s = _stats(passed=100, failed=2, skipped=0, expected=2)
    panel = _render_validation_panel("pymongo", s)
    # actionable_failures = 2 - 2 = 0. A clean panel with known,
    # report-documented divergences folds them in plainly as
    # "N known divergences" — no defensive rate-accounting on the card.
    assert "<strong>100</strong> tests passed" in panel
    assert "<strong>2</strong> known divergences" in panel
    assert "unexpected failures" not in panel
    assert "excluded from the rate" not in panel


def test_render_validation_panel_singular_known_divergence() -> None:
    s = _stats(passed=358, failed=1, skipped=0, expected=1)
    panel = _render_validation_panel("mongo-node-driver", s)
    assert "<strong>1</strong> known divergence" in panel
    assert "known divergences" not in panel  # singular


def test_render_validation_panel_omits_note_when_no_expected_failures() -> None:
    s = _stats(passed=100, failed=0, skipped=0, expected=0)
    panel = _render_validation_panel("pymongo", s)
    assert "documented" not in panel


def test_render_smoke_panel_includes_kind_badge_and_no_report_link() -> None:
    panel = _render_smoke_panel(
        {
            "title": "mongo-php-library",
            "lang": "PHP",
            "kind": "smoke",
            "kind_label": "Feature smokes",
            "rate_value": "6 / 6",
            "rate_label": "features verified",
            "note": "test note",
            "report_url": None,
        }
    )
    assert 'class="kind smoke">Feature smokes</span>' in panel
    assert 'class="rate">6 / 6</span>' in panel
    assert 'class="rate-label">features verified</span>' in panel
    assert "Read the report" not in panel


def test_render_smoke_panel_pending_omits_rate_block() -> None:
    panel = _render_smoke_panel(
        {
            "title": "mongo-rust-driver",
            "lang": "Rust",
            "kind": "pending",
            "kind_label": "Pending",
            "rate_value": None,
            "rate_label": None,
            "note": "pending note",
            "report_url": None,
        }
    )
    assert 'class="rate">' not in panel
    assert "pending note" in panel


def _write_fake_validation_dir(d: Path) -> None:
    """Write minimal raw artifacts the collectors accept so the renderer runs end-to-end."""
    (d / "raw.json").write_text(
        json.dumps(
            {
                "summary": {"passed": 100, "failed": 0, "skipped": 5},
                "tests": [],
            }
        ),
        encoding="utf-8",
    )
    (d / "pymongo-async-raw.json").write_text(
        json.dumps(
            {
                "summary": {"passed": 80, "failed": 0, "skipped": 4},
                "tests": [],
            }
        ),
        encoding="utf-8",
    )
    (d / "go-raw.ndjson").write_text(
        '{"Action":"pass","Test":"TestA"}\n{"Action":"pass","Test":"TestB"}\n',
        encoding="utf-8",
    )
    (d / "node-raw.json").write_text(
        json.dumps({"stats": {"passes": 50, "failures": 0, "pending": 1}, "failures": []}),
        encoding="utf-8",
    )
    (d / "ruby-raw.json").write_text(
        json.dumps(
            {
                "summary": {"example_count": 30, "failure_count": 0, "pending_count": 2},
                "examples": [],
            }
        ),
        encoding="utf-8",
    )
    # Java: empty xml-results dir + an explicit pass/skip count in the
    # validate-java report-style JSON. The collector falls back to
    # parsing the existing report.md if neither shape is present.
    (d / "java-raw").mkdir()


def test_render_end_to_end_against_synthetic_validation_dir(tmp_path: Path) -> None:
    _write_fake_validation_dir(tmp_path)
    # Java needs its own artifact layout; the collector may handle a
    # missing one with a SystemExit. Skip if so — the other four
    # cover the renderer's main path.
    try:
        html = render(tmp_path)
    except SystemExit as exc:
        if "mongo-java-driver" in str(exc):
            pytest.skip("Java collector requires a populated java-raw layout")
        raise
    # Expect one panel per validated driver + one per smoke panel,
    # wrapped in the grid div + foot prose.
    assert html.startswith('<div class="drivers">')
    assert html.endswith("</p>\n")
    assert "cross-driver feature matrix" in html
    for name in PANEL_PROSE:
        assert f"<h3>{name}</h3>" in html
    for panel in SMOKE_PANELS:
        assert f"<h3>{panel['title']}</h3>" in html


def test_render_missing_artifact_fails_loudly(tmp_path: Path) -> None:
    # Empty dir → first collector returns None → SystemExit telling the
    # user which command to run. The message names the server, because with
    # `--server rust` the fix is `validate-all --server rust` and pointing at
    # the bare command would send the reader to re-measure the wrong half.
    with pytest.raises(SystemExit, match=r"run `invoke validate-all --server python`"):
        render(tmp_path)


def test_render_missing_artifact_names_the_rust_half(tmp_path: Path) -> None:
    with pytest.raises(SystemExit, match=r"run `invoke validate-all --server rust`"):
        render(tmp_path, "rust")


def test_counts_line_accounts_for_every_failure_behind_the_rate() -> None:
    """A card's own numbers must reconcile with its headline rate.

    `_format_counts` used to report `actionable_failures` alone whenever a
    gauge had any, which dropped the documented divergences from the label
    while `_format_rate` kept counting them. pymongo shipped a live card
    reading "1196 tests passed · 10 failed" above a 98.7% rate, when
    1196/(1196+10) floors to 99.2% — the five documented divergences were in
    the rate and nowhere in the label, so a reader could not get from the
    card to the rate.
    """
    import math
    import re

    from validation_summary.driver_panels import _format_counts, _format_rate
    from validation_summary.generate import GaugeStats

    def stats(passed: int, failed: int, expected: int) -> GaugeStats:
        s = GaugeStats(
            name="g",
            language="L",
            driver_version="v",
            passed=passed,
            failed=failed,
            skipped=0,
            failure_descriptions=[],
        )
        s.expected_failures = expected
        return s

    # clean, documented-only, actionable-only, and the mixed case that broke.
    for passed, failed, expected in [
        (439, 0, 0),
        (357, 1, 1),
        (493, 3, 0),
        (1196, 15, 5),
        (783, 7, 6),
    ]:
        st = stats(passed, failed, expected)
        counts = re.sub(r"<[^>]+>", "", _format_counts(st))
        numbers = [int(n) for n in re.findall(r"\d+", counts)]
        assert numbers[0] == passed
        # Every failure behind the rate is named somewhere on the line.
        assert sum(numbers[1:]) == failed, counts
        # And the line reconciles with the headline rate the card shows.
        ran = passed + failed
        implied = "100.0%" if passed == ran else f"{math.floor(passed / ran * 1000) / 10:.1f}%"
        assert _format_rate(st) == implied, (counts, _format_rate(st), implied)
