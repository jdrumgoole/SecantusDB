"""Tests for task time estimates (observed history > declared guess)."""

from __future__ import annotations

from pathlib import Path

from secantus.jobkit import Journal
from secantus.opsboard.estimates import estimate_for, format_duration


def test_format_duration() -> None:
    assert format_duration(9) == "9s"
    assert format_duration(59) == "59s"
    assert format_duration(60) == "1m 00s"
    assert format_duration(200) == "3m 20s"
    assert format_duration(3660) == "1h 01m"


def test_declared_estimate_is_labelled_rough() -> None:
    est = estimate_for([], declared_seconds=300)
    assert est.text == "5m 00s"
    assert est.source == "rough"
    assert "rough estimate" in est.qualifier
    assert "no successful runs recorded" in est.qualifier


def test_no_data_at_all_is_unknown() -> None:
    est = estimate_for([], declared_seconds=0)
    assert est.source == "unknown"
    assert est.text == "unknown"


def test_observed_median_beats_declared() -> None:
    # Declared says 5m but this machine actually takes ~10s.
    est = estimate_for([10.0, 12.0, 8.0], declared_seconds=300)
    assert est.source == "measured"
    assert est.text == "10s"  # median of 8/10/12
    assert est.samples == 3
    assert "median of the last 3" in est.qualifier


def test_few_samples_are_flagged_as_such() -> None:
    est = estimate_for([42.0], declared_seconds=300)
    assert est.source == "measured"
    assert est.samples == 1
    assert "only 1 previous run" in est.qualifier


# --------------------------------------------------------------------------- #
# Journal duration history
# --------------------------------------------------------------------------- #


def _finished(journal: Journal, argv: list[str], *, dur: float, code: int) -> None:
    jid = journal.create(
        target="python", task=argv[0], argv=argv, worktree="/w", host_pid=1, started_at=1000.0
    )
    journal.finish(jid, code, ended_at=1000.0 + dur)


def test_completed_durations_only_counts_passed_runs(tmp_path: Path) -> None:
    journal = Journal(tmp_path / "opsboard.db")
    _finished(journal, ["test"], dur=100.0, code=0)
    _finished(journal, ["test"], dur=120.0, code=0)
    _finished(journal, ["test"], dur=3.0, code=1)  # failed early — must be excluded

    durations = journal.completed_durations(["test"])
    assert sorted(durations) == [100.0, 120.0]


def test_completed_durations_matches_argv_exactly(tmp_path: Path) -> None:
    journal = Journal(tmp_path / "opsboard.db")
    _finished(journal, ["validate", "--server", "python"], dur=50.0, code=0)
    _finished(journal, ["validate", "--server", "rust"], dur=90.0, code=0)

    assert journal.completed_durations(["validate", "--server", "python"]) == [50.0]
    assert journal.completed_durations(["validate", "--server", "rust"]) == [90.0]


def test_completed_durations_respects_limit(tmp_path: Path) -> None:
    journal = Journal(tmp_path / "opsboard.db")
    for i in range(10):
        _finished(journal, ["test"], dur=float(i + 1), code=0)
    assert len(journal.completed_durations(["test"], limit=4)) == 4


def test_completed_durations_empty_for_unknown_task(tmp_path: Path) -> None:
    journal = Journal(tmp_path / "opsboard.db")
    assert journal.completed_durations(["never-run"]) == []
