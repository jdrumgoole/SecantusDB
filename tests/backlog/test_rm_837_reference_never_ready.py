"""RM-837: a reference mongod that never becomes ready is a failure.

`tests/mongod_reference.py::_start_mongod_once` used to call
`pytest.skip("mongod did not become ready")` when mongod never answered, which
hid a reference-server crash or hang at startup as a skipped comparison. A
test that does not complete is a failure (CLAUDE.md), so it raises, carrying
the tail of mongod's own log, and still reaps the process and its dbpath.

These tests stand in a script for mongod, so they need no mongod on the box.
"""

from __future__ import annotations

import os
import stat
import tempfile
import time
from pathlib import Path
from typing import Any

import pytest

from tests import mongod_reference


def _fake_mongod(tmp_path: Path, body: str) -> str:
    script = tmp_path / "mongod"
    script.write_text(f"#!/bin/sh\n{body}\n")
    script.chmod(script.stat().st_mode | stat.S_IXUSR)
    return str(script)


def _probe_stores() -> set[str]:
    prefix = f"{mongod_reference.MONGOD_TMP_PREFIX}{os.getpid()}-mongod-"
    return {p for p in os.listdir(tempfile.gettempdir()) if p.startswith(prefix)}


@pytest.mark.backlog_fixed("RM-837")
def test_a_mongod_that_exits_during_startup_fails_with_its_log(
    tmp_path: Path, monkeypatch: Any
) -> None:
    """A mongod that dies while it starts fails the test with its exit status
    and the last of its log, and leaves no dbpath behind."""
    monkeypatch.setattr(
        mongod_reference,
        "MONGOD",
        _fake_mongod(tmp_path, 'echo "Fatal assertion 28595 boom" >&2; exit 3'),
    )
    before = _probe_stores()
    with pytest.raises(RuntimeError) as failure:
        mongod_reference._start_mongod_once()
    message = str(failure.value)
    assert "returncode=3" in message, message
    assert "Fatal assertion 28595 boom" in message, message
    assert _probe_stores() == before


@pytest.mark.backlog_fixed("RM-837")
def test_a_mongod_that_never_answers_fails_with_its_log(tmp_path: Path, monkeypatch: Any) -> None:
    """A mongod that stays up and never answers fails the test, with its log,
    and is terminated and its dbpath removed."""
    monkeypatch.setattr(
        mongod_reference,
        "MONGOD",
        _fake_mongod(tmp_path, 'echo "waiting for something that never comes" >&2; exec sleep 120'),
    )
    # The 30 s readiness wait is not what is under test: its answer is
    # "nothing answered", which `_wait_owned` returns once its time is up.
    # (After the half second the script needs to write its line.)
    monkeypatch.setattr(
        mongod_reference, "_wait_owned", lambda proc, port, timeout: time.sleep(0.5) or False
    )
    before = _probe_stores()
    with pytest.raises(RuntimeError) as failure:
        mongod_reference._start_mongod_once()
    message = str(failure.value)
    assert "never answered" in message, message
    assert "waiting for something that never comes" in message, message
    assert _probe_stores() == before
