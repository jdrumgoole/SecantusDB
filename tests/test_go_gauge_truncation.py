"""A Go gauge run the test binary never finished must not read as a result.

`go test` killing itself on `-timeout` panics the binary WITHOUT emitting a
terminal event for the tests still in flight. The summariser therefore saw only
the tests that finished, counted zero failures among them, and printed 100.0% —
while the package-level event said `fail`. The two disagreed and nothing looked
at the disagreement.

That is not hypothetical. Every recorded run of this gauge — both servers, and
the committed 2026-09-21 report — stopped at 476 of 481 tests after a 30-minute
hang in `TestInitialDNSSeedlistDiscoverySpec` (a DNS resolver test that never
contacts SecantusDB), and every one of them published 100.0%. Two artifacts from
different servers a week apart agreeing exactly reads as confirmation; it meant
both were cut off at the same hang.

`tests/test_pgjdbc_gauge_truncation.py` is the same guard for the pgjdbc gauge;
this one existed nowhere for Go.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from go_validation import generate_report  # noqa: E402

PKG = "go.mongodb.org/mongo-driver/v2/internal/integration"


def _ndjson(path: Path, events: list[dict]) -> Path:
    path.write_text("\n".join(json.dumps(e) for e in events) + "\n")
    return path


def _complete() -> list[dict]:
    return [
        {"Action": "run", "Package": PKG, "Test": "TestOne"},
        {"Action": "pass", "Package": PKG, "Test": "TestOne"},
        {"Action": "run", "Package": PKG, "Test": "TestTwo"},
        {"Action": "skip", "Package": PKG, "Test": "TestTwo"},
        {"Action": "pass", "Package": PKG},
    ]


def test_a_complete_run_is_not_flagged(tmp_path):
    raw = _ndjson(tmp_path / "raw.ndjson", _complete())
    truncated = generate_report.render(raw, tmp_path / "out.md")
    assert truncated is False
    assert "TRUNCATED" not in (tmp_path / "out.md").read_text()


def test_a_test_that_started_and_never_finished_is_truncation(tmp_path):
    """The exact shape of the real failure: a `run` with no terminal event."""
    events = _complete()[:-1] + [
        {"Action": "run", "Package": PKG, "Test": "TestInitialDNSSeedlistDiscoverySpec"},
        {"Action": "fail", "Package": PKG},  # package died; no test-level fail
    ]
    raw = _ndjson(tmp_path / "raw.ndjson", events)
    out = tmp_path / "out.md"
    assert generate_report.render(raw, out) is True
    body = out.read_text()
    assert "TRUNCATED" in body
    assert "TestInitialDNSSeedlistDiscoverySpec" in body


def test_package_fail_with_no_failing_test_is_truncation(tmp_path):
    """A binary that died without accounting for its tests.

    Every test that started also finished, so the hung-test check alone would
    pass this — the package-level `fail` is the only signal left, and it is the
    one that disagrees with a 100% rate.
    """
    events = _complete()[:-1] + [{"Action": "fail", "Package": PKG}]
    raw = _ndjson(tmp_path / "raw.ndjson", events)
    out = tmp_path / "out.md"
    assert generate_report.render(raw, out) is True
    assert "no failing test beneath them" in out.read_text()


def test_a_real_test_failure_is_not_truncation(tmp_path):
    """A package `fail` WITH a failing test under it is an ordinary red run.

    The guard must not cry truncation at every failing gauge, or it will be
    ignored exactly when it matters.
    """
    events = [
        {"Action": "run", "Package": PKG, "Test": "TestBoom"},
        {"Action": "fail", "Package": PKG, "Test": "TestBoom"},
        {"Action": "fail", "Package": PKG},
    ]
    raw = _ndjson(tmp_path / "raw.ndjson", events)
    out = tmp_path / "out.md"
    assert generate_report.render(raw, out) is False
    assert "TRUNCATED" not in out.read_text()


def test_the_banner_precedes_the_numbers(tmp_path):
    """Placement is the point: a reader who meets the table first has already
    formed a view of the pass rate before any caveat reaches them."""
    events = [
        {"Action": "run", "Package": PKG, "Test": "TestHangs"},
        {"Action": "fail", "Package": PKG},
    ]
    raw = _ndjson(tmp_path / "raw.ndjson", events)
    out = tmp_path / "out.md"
    generate_report.render(raw, out)
    body = out.read_text()
    assert body.index("TRUNCATED") < body.index("| **Overall**")
