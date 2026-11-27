"""The reference mongod's port race (RM-834, found in CI run 45974752150,
2026-11-27): `_free_port()` closes its probe socket before the mongod binds,
and mongod opens WiredTiger BEFORE it binds, so another xdist worker's
mongod can take the port while ours is still starting. The old guard asked
only whether OUR process had exited; it had not yet, the ping reached the
other worker's standalone mongod, and 108 tests errored at setup
(`replSetInitiate`: "This node was not started with replication enabled").
Worse, a differential gate could have compared against a server it does not
own and called it agreement.

The harness now asks the answering server who it is (`serverStatus().pid`)
and takes another port when that is not the mongod it spawned.
"""

from __future__ import annotations

import subprocess
import sys
from collections.abc import Iterator

import pytest

from tests import mongod_reference
from tests.mongod_reference import (
    MONGOD,
    _answering_pid,
    _host_port,
    _LostPortRace,
    _start_mongod,
    _start_mongod_once,
    _stop_mongod,
    _wait_owned,
)


@pytest.fixture
def foreign() -> Iterator[tuple[subprocess.Popen, str, int]]:
    """A standalone mongod belonging to somebody else: `(proc, dbpath, port)`."""
    assert MONGOD, "no mongod on PATH: the reference server is required"
    proc, uri, dbpath = _start_mongod()
    try:
        yield proc, dbpath, _host_port(uri)[1]
    finally:
        _stop_mongod(proc, dbpath)


def _port_handed_out_first(monkeypatch: pytest.MonkeyPatch, port: int) -> list[int]:
    """Make `_free_port()` hand out `port` once (as a probe that lost the
    race would), then real free ports. Returns the ports it handed out."""
    real = mongod_reference._free_port
    handed: list[int] = []

    def free_port() -> int:
        handed.append(port if not handed else real())
        return handed[-1]

    monkeypatch.setattr(mongod_reference, "_free_port", free_port)
    return handed


@pytest.mark.backlog_fixed("RM-834")
def test_a_mongod_that_lost_its_port_is_refused_not_trusted(
    foreign: tuple[subprocess.Popen, str, int], monkeypatch: pytest.MonkeyPatch
) -> None:
    """One attempt on a port a foreign mongod holds raises `_LostPortRace`:
    the ping is answered, but not by the mongod this harness spawned."""
    other, _, port = foreign
    _port_handed_out_first(monkeypatch, port)
    with pytest.raises(_LostPortRace):
        _start_mongod_once(None, ("--replSet", "backlog"))
    assert _answering_pid(port) == other.pid


@pytest.mark.backlog_fixed("RM-834")
def test_the_retry_takes_another_port_and_returns_its_own_mongod(
    foreign: tuple[subprocess.Popen, str, int], monkeypatch: pytest.MonkeyPatch
) -> None:
    """`_start_mongod` goes on to a fresh port, and what it returns is the
    server answering there: its pid is the returned process's, never the
    foreign mongod's."""
    other, _, port = foreign
    handed = _port_handed_out_first(monkeypatch, port)
    proc, uri, dbpath = _start_mongod(extra_args=("--replSet", "backlog"))
    try:
        mine = _host_port(uri)[1]
        assert mine != port
        assert len(handed) >= 2
        assert _answering_pid(mine) == proc.pid != other.pid
        assert _answering_pid(port) == other.pid  # untouched
    finally:
        _stop_mongod(proc, dbpath)


@pytest.mark.backlog_fixed("RM-834")
def test_a_live_process_is_not_proof_of_ownership(
    foreign: tuple[subprocess.Popen, str, int],
) -> None:
    """The failing shape itself: our process has NOT exited (it is still
    "starting") while another server answers on the port. The old guard,
    `proc.poll() is not None`, passed here."""
    _, _, port = foreign
    starting = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
    try:
        with pytest.raises(_LostPortRace):
            _wait_owned(starting, port, 2)
        assert starting.poll() is None  # alive throughout: the old test said "ours"
        with pytest.raises(_LostPortRace):
            mongod_reference._assert_owns_port(starting, port)
    finally:
        starting.kill()
        starting.wait()


@pytest.mark.backlog_fixed("RM-834")
def test_four_lost_races_are_still_an_error(
    foreign: tuple[subprocess.Popen, str, int], monkeypatch: pytest.MonkeyPatch
) -> None:
    """No retry hides a persistent failure: four ports in a row held by
    someone else raise, and every spawned mongod is reaped."""
    _, _, port = foreign
    monkeypatch.setattr(mongod_reference, "_free_port", lambda: port)
    with pytest.raises(_LostPortRace):
        _start_mongod(extra_args=("--replSet", "backlog"))
