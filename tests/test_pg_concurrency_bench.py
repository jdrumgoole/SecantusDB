"""``bench/pg_concurrency.py`` must stay importable and honest.

The benchmark itself needs two servers and half a minute, so it does not run
here. What is pinned is the part that has silently rotted in bench scripts
before: the module importing at all, and the summary maths being right -- a
median or a scaling ratio computed wrongly produces a plausible number, which
is worse than a crash.
"""

from __future__ import annotations

import importlib

import pytest

bench = importlib.import_module("bench.pg_concurrency")


def test_rate_is_committed_over_elapsed() -> None:
    assert bench.Result(clients=4, committed=1_000, seconds=2.0).ops_per_s == 500.0


def test_a_zero_length_run_does_not_divide_by_zero() -> None:
    assert bench.Result(clients=1, committed=0, seconds=0.0).ops_per_s == 0.0


def test_median_not_mean_so_one_slow_trial_cannot_move_the_figure() -> None:
    """An unrelated process on the box produces exactly this shape."""
    trials = bench.Trials(clients=4, rates=[1000.0, 1010.0, 200.0])
    assert trials.median == 1000.0


def test_spread_reports_peak_to_peak_against_the_median() -> None:
    trials = bench.Trials(clients=2, rates=[100.0, 110.0, 105.0])
    assert trials.spread_pct == pytest.approx(100.0 * 10.0 / 105.0)


def test_a_single_trial_has_no_spread_to_report() -> None:
    assert bench.Trials(clients=1, rates=[42.0]).spread_pct == 0.0


def test_the_release_binary_is_what_the_module_points_at() -> None:
    """A debug binary is ~2.3x slower and has inverted a conclusion here."""
    assert bench.RUST_BINARY.name == "secantusd-pg"
    assert "release" in bench.RUST_BINARY.parts, bench.RUST_BINARY
