"""End-to-end mongostat / mongotop against an embedded SecantusDB.

Both tools poll admin commands (serverStatus and top respectively) and
are built on mongo-go-driver: missing or mis-typed reply fields surface
as Go panics or hard errors rather than the silent tolerance pymongo
shows. A single-iteration run of each is a cheap conformance probe of
the whole reply shape.

The tests skip gracefully if the tools aren't on PATH so they don't
break local runs without the MongoDB Database Tools installed (CI image
must install them explicitly).
"""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest

from secantus import SecantusDBServer

MONGOSTAT = shutil.which("mongostat")
MONGOTOP = shutil.which("mongotop")


def _run(cmd: list[str]) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
    if result.returncode != 0:
        raise AssertionError(
            f"{cmd[0]} exited with code {result.returncode}\n"
            f"--- stdout ---\n{result.stdout}\n"
            f"--- stderr ---\n{result.stderr}"
        )
    return result


@pytest.mark.skipif(MONGOSTAT is None, reason="mongostat not on PATH")
def test_mongostat_single_iteration(tmp_path: Path) -> None:
    wt_dir = tmp_path / "secantus-wt"
    wt_dir.mkdir()
    with SecantusDBServer(port=0, storage_path=str(wt_dir)) as server:
        assert MONGOSTAT is not None  # narrowed by skipif
        result = _run([MONGOSTAT, f"--uri={server.uri}", "-n", "1"])
        # One header line plus one stat line naming the standard columns.
        assert "insert" in result.stdout
        assert "query" in result.stdout


@pytest.mark.skipif(MONGOTOP is None, reason="mongotop not on PATH")
def test_mongotop_single_iteration(tmp_path: Path) -> None:
    wt_dir = tmp_path / "secantus-wt"
    wt_dir.mkdir()
    with SecantusDBServer(port=0, storage_path=str(wt_dir)) as server:
        assert MONGOTOP is not None  # narrowed by skipif
        result = _run([MONGOTOP, f"--uri={server.uri}", "-n", "1", "1"])
        assert "ns" in result.stdout
        assert "total" in result.stdout
