"""`psycopg_validation.check_run`: a truncated gauge run is a failure, and a
TLS run is held to the plain one."""

from __future__ import annotations

import json
from pathlib import Path

import pytest
from psycopg_validation import check_run


def _report(path: Path, outcomes: dict[str, str], *, collected: int | None = None) -> Path:
    tests = [{"nodeid": n, "outcome": o} for n, o in outcomes.items()]
    summary = {"collected": len(tests) if collected is None else collected, "deselected": 0}
    path.write_text(json.dumps({"summary": summary, "tests": tests}))
    return path


def test_a_complete_run_passes_even_with_failing_tests(tmp_path: Path) -> None:
    raw = _report(tmp_path / "a.json", {"t::a": "passed", "t::b": "failed"})
    assert check_run.main([str(raw)]) == 0


def test_a_missing_report_is_a_truncated_run(tmp_path: Path) -> None:
    assert check_run.main([str(tmp_path / "absent.json")]) == 1


def test_fewer_outcomes_than_started_is_a_truncated_run(tmp_path: Path) -> None:
    raw = _report(tmp_path / "a.json", {"t::a": "passed"}, collected=3)
    with pytest.raises(check_run.Truncated, match="started 3 tests, 1 reported"):
        check_run.load(raw)
    assert check_run.main([str(raw)]) == 1


def test_a_run_that_started_nothing_is_not_a_pass(tmp_path: Path) -> None:
    assert check_run.main([str(_report(tmp_path / "a.json", {}))]) == 1


def test_deselected_tests_are_not_counted_as_started(tmp_path: Path) -> None:
    raw = tmp_path / "a.json"
    raw.write_text(
        json.dumps(
            {
                "summary": {"collected": 3, "deselected": 2},
                "tests": [{"nodeid": "t::a", "outcome": "passed"}],
            }
        )
    )
    assert check_run.main([str(raw)]) == 0


def test_a_failure_over_tls_only_fails_the_comparison(tmp_path: Path) -> None:
    plain = _report(tmp_path / "p.json", {"t::a": "passed", "t::b": "failed"})
    same = _report(tmp_path / "s.json", {"t::a": "passed", "t::b": "failed"})
    worse = _report(tmp_path / "w.json", {"t::a": "error", "t::b": "failed"})
    assert check_run.main([str(plain), "--tls", str(same)]) == 0
    assert check_run.main([str(plain), "--tls", str(worse)]) == 1
    assert check_run.tls_only_failures(check_run.load(plain), check_run.load(worse)) == ["t::a"]


def test_the_two_runs_must_have_run_the_same_tests(tmp_path: Path) -> None:
    plain = _report(tmp_path / "p.json", {"t::a": "passed", "t::b": "passed"})
    tls = _report(tmp_path / "t.json", {"t::a": "passed", "t::c": "passed"})
    assert check_run.main([str(plain), "--tls", str(tls)]) == 1


def test_a_truncated_tls_run_fails_the_comparison(tmp_path: Path) -> None:
    plain = _report(tmp_path / "p.json", {"t::a": "passed"})
    assert check_run.main([str(plain), "--tls", str(tmp_path / "absent.json")]) == 1
