"""A pymongo gauge run that produced no results must not publish a report.

The two pymongo tasks run pytest with `warn=True` — correctly, because a
partially-red suite still owes us a report — and then generated the report
UNCONDITIONALLY from whatever raw artifact was on disk. Unlike `_run_gauge`,
they never cleared that artifact first. So a run that collected ZERO tests
rewrote the report from a previous run's data, stamped with today's date and
the current version string.

Observed 2026-09-28: `invoke validate --server rust` died in one second on a
missing `_secantus_server` and published "Generated 2026-09-28 — SecantusDB
0.6.0b17, 99.4%" over a raw artifact from 30 August. The figures had even
drifted against the previous report (99.5% -> 99.4%, with a new failing test)
because the generator had changed under the same data — so diffing the file
would have suggested a fresh run had caught a regression.

`tests/test_go_gauge_truncation.py` guards the neighbouring failure (a run that
stopped part-way); this one guards the run that never started.
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import tasks  # noqa: E402


def test_clear_raw_removes_a_previous_runs_artifact(tmp_path):
    raw = tmp_path / "nested" / "raw.json"
    raw.parent.mkdir()
    raw.write_text('{"stale": true}')
    tasks._clear_raw(str(raw))
    assert not raw.exists(), "a previous run's raw must not survive into this one"


def test_clear_raw_creates_the_directory_and_tolerates_absence(tmp_path):
    raw = tmp_path / "fresh" / "raw.json"
    tasks._clear_raw(str(raw))  # must not raise
    assert raw.parent.is_dir()


def test_require_fresh_raw_accepts_a_real_artifact(tmp_path):
    raw = tmp_path / "raw.json"
    raw.write_text('{"tests": []}')
    tasks._require_fresh_raw(str(raw), "pymongo gauge")  # must not raise


def test_require_fresh_raw_refuses_a_missing_artifact(tmp_path):
    with pytest.raises(SystemExit) as exc:
        tasks._require_fresh_raw(str(tmp_path / "absent.json"), "pymongo gauge")
    msg = str(exc.value)
    assert "NOT regenerating the report" in msg
    assert "pymongo gauge" in msg


def test_require_fresh_raw_refuses_an_empty_artifact(tmp_path):
    """The crash shape: pytest touched the file and wrote nothing."""
    raw = tmp_path / "raw.json"
    raw.write_text("")
    with pytest.raises(SystemExit):
        tasks._require_fresh_raw(str(raw), "pymongo gauge")


def test_the_refusal_names_the_usual_cause(tmp_path):
    """A zero-second run is almost always the missing embedded extension, so
    the message says which command rebuilds it rather than leaving the reader
    to rediscover that."""
    with pytest.raises(SystemExit) as exc:
        tasks._require_fresh_raw(str(tmp_path / "absent.json"), "pymongo gauge")
    assert "rust-server-build" in str(exc.value)


def test_both_pymongo_tasks_clear_and_guard_their_raw():
    """Pins the wiring, not just the helpers.

    The helpers are useless if a task forgets to call them, and that is exactly
    how the gauges diverged from `_run_gauge` in the first place.
    """
    source = Path(tasks.__file__).read_text()
    assert source.count("_clear_raw(raw_json)") == 2, "sync + async must both clear"
    assert source.count("_require_fresh_raw(raw_json,") == 2, "both must guard"
