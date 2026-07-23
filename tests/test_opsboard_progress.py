"""Unit tests for the log→progress parser (``secantus.opsboard.progress``)."""

from __future__ import annotations

from secantus.opsboard.progress import ACTIVE, DONE, FAILED, PENDING, parse_progress


def test_no_signal_is_indeterminate() -> None:
    p = parse_progress("just some output\nwith no markers\n")
    assert p.overall is None
    assert p.determinate is False
    assert p.has_phases is False


def test_pytest_percent_drives_overall() -> None:
    log = "tests/a.py::x PASSED [ 12%]\ntests/b.py::y PASSED [ 48%]\n"
    p = parse_progress(log)
    assert p.percent == 48
    assert p.overall == 48
    assert p.determinate is True
    assert p.has_phases is False


def test_phase_markers_build_stepper() -> None:
    log = "==> [1/3] Lint\nsome lint\n==> [2/3] Tests\ntests/a.py::x PASSED [ 50%]\n"
    p = parse_progress(log)
    assert p.total_phases == 3
    assert p.current_phase == 2
    assert [ph.state for ph in p.phases] == [DONE, ACTIVE, PENDING]
    assert [ph.label for ph in p.phases] == ["Lint", "Tests", "…"]
    # overall = (1 completed + 0.50 of the active) / 3 ≈ 50%
    assert p.overall == 50
    assert p.determinate is True


def test_known_labels_prefill_before_markers() -> None:
    p = parse_progress("", known_labels=["Lint", "Tests", "Perf"])
    assert p.total_phases == 3
    assert [ph.label for ph in p.phases] == ["Lint", "Tests", "Perf"]
    assert all(ph.state == PENDING for ph in p.phases)


def test_markers_win_count_over_known_labels() -> None:
    # Task ran only 2 steps (e.g. gate --no-perf) though 3 labels are declared.
    log = "==> [1/2] Lint\n==> [2/2] Tests\n"
    p = parse_progress(log, known_labels=["Lint", "Tests", "Perf"])
    assert p.total_phases == 2
    assert [ph.label for ph in p.phases] == ["Lint", "Tests"]


def test_done_passed_marks_all_done_and_full_bar() -> None:
    log = "==> [1/3] Lint\n==> [2/3] Tests\n==> [3/3] Perf\n"
    p = parse_progress(log, done=True, passed=True)
    assert all(ph.state == DONE for ph in p.phases)
    assert p.overall == 100


def test_done_failed_marks_current_phase_failed() -> None:
    log = "==> [1/3] Lint\n==> [2/3] Tests\n"
    p = parse_progress(log, done=True, passed=False)
    assert [ph.state for ph in p.phases] == [DONE, FAILED, PENDING]
    assert p.overall != 100


def test_overall_clamped() -> None:
    p = parse_progress("[ 250%]")
    assert p.overall == 100
