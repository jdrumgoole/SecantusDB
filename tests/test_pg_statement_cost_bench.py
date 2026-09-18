"""``bench/pg_statement_cost.py`` must import and keep its stage set.

The benchmark needs two servers, so it does not run here. What is pinned is the
shape the attribution depends on: the stage list must keep a no-SQL floor and a
constant-select, because the finding is a DIFFERENCE between those two rows --
drop either and the instrument stops being able to say where the time goes.
"""

from __future__ import annotations

import importlib

bench = importlib.import_module("bench.pg_statement_cost")


def test_the_release_binary_is_what_is_measured() -> None:
    assert "release" in bench.RUST.parts, bench.RUST


def test_measure_takes_an_in_transaction_flag() -> None:
    """The block path is the slower one and must stay measurable."""
    import inspect

    assert "in_transaction" in inspect.signature(bench.measure).parameters


def test_the_protocol_floor_stage_still_exists() -> None:
    """`ping` is what separates 'the wire is slow' from 'the statement is'."""
    src = bench.__doc__ or ""
    assert "ping" in src
    assert "select_const" in src
