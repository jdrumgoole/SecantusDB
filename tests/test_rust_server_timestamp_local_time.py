"""The Rust SERVER renders `$toLower` of a `Timestamp` in the process's local zone.

`test_tolower_timestamp_local_time.py` pins the rendering through the
`_secantus_core` binding, but no Windows CI lane has that binding -- so when the
Rust engine ignored `TZ` on Windows (it read the host zone through
`chrono::Local`, which on Windows never consults `TZ`), nothing in CI could see
it. This file drives the real Rust server through pymongo instead, which needs
only `_secantus_server`, and that IS built on Windows by the `storage-engine`
job. That job also moves the runner's host zone off UTC, which is the other half
of the fix: on a UTC host, "honours `TZ=UTC`" and "ignores `TZ`" give the same
answer.

Every expectation is either a literal mongod 8.2.11 answer (shared with the
sibling file) or, with `TZ` unset, the host's own local time as the C runtime
resolves it -- which is exactly what mongod reads on each platform.
"""

from __future__ import annotations

import os
import subprocess
import sys

import pytest

from tests.test_tolower_timestamp_local_time import MEASURED

pytest.importorskip("_secantus_server")
pytest.importorskip("pymongo")

_RENDER = """
import sys
import pymongo
from bson import Timestamp
import _secantus_server

srv = _secantus_server.RustServer(sys.argv[1], 0)
try:
    host, port = srv.address
    client = pymongo.MongoClient(host, port, directConnection=True)
    coll = client["t"]["c"]
    coll.insert_one({"_id": 1, "ts": Timestamp(int(sys.argv[2]), int(sys.argv[3]))})
    out = list(coll.aggregate([{"$project": {"_id": 0, "r": {"$toLower": "$ts"}}}]))
    print(out[0]["r"])
    client.close()
finally:
    srv.stop()
"""


def _render(tmp_path, tz: str | None, secs: int, inc: int) -> str:
    env = dict(os.environ)
    env.pop("TZ", None)
    if tz is not None:
        env["TZ"] = tz
    out = subprocess.run(
        [sys.executable, "-c", _RENDER, str(tmp_path / "wt"), str(secs), str(inc)],
        capture_output=True,
        text=True,
        env=env,
        check=True,
        timeout=120,
    )
    return out.stdout.strip()


@pytest.mark.parametrize(("tz", "secs", "inc", "expected"), MEASURED)
def test_rust_server_matches_mongod(tmp_path, tz: str, secs: int, inc: int, expected: str) -> None:
    assert _render(tmp_path, tz, secs, inc) == expected


def _host_local(secs: int, inc: int, tz: str | None = None) -> str:
    """What mongod renders with `TZ` unset: the host zone, via the C runtime.

    Computed in a subprocess with `TZ` removed, so it is the zone the server
    subprocess will see rather than whatever this pytest process inherited.
    """
    script = (
        "import sys, time\n"
        "t = time.localtime(int(sys.argv[1]))\n"
        "print(time.strftime('%b', t).lower(), '%2d' % t.tm_mday,"
        " time.strftime('%H:%M:%S', t) + ':' + sys.argv[2])\n"
    )
    env = {k: v for k, v in os.environ.items() if k != "TZ"}
    if tz is not None:
        env["TZ"] = tz
    out = subprocess.run(
        [sys.executable, "-c", script, str(secs), str(inc)],
        capture_output=True,
        text=True,
        env=env,
        check=True,
    )
    return out.stdout.strip()


@pytest.mark.parametrize(
    ("secs", "inc"),
    [(1700000000, 3), (1720000000, 0)],  # November and July: both sides of DST
)
def test_rust_server_uses_the_host_zone_when_tz_is_unset(tmp_path, secs: int, inc: int) -> None:
    assert _render(tmp_path, None, secs, inc) == _host_local(secs, inc)


def test_host_local_rendering_matches_the_measured_format() -> None:
    """Self-check for `_host_local`: under `TZ=UTC` it must equal mongod's literal.

    Without this the unset-`TZ` test above would compare two renderings that
    could share a format bug.
    """
    assert _host_local(1720000000, 0, "UTC") == "jul  3 09:46:40:0"
    assert _host_local(1767225600, 12, "UTC") == "jan  1 00:00:00:12"
