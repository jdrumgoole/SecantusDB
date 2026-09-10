"""`$toLower` / `$toUpper` of a `Timestamp`, which mongod renders in LOCAL time.

mongod puts a `Timestamp` through a legacy `asctime`-like path rather than the
`$dateToString` format language, and it does so in the **server process's local
timezone**. Measured against mongod 8.2.11 on 2026-09-10 by running the server
under three zones:

===================  ================================================
`TZ`                 `{$toLower: Timestamp(1700000000, 3)}`
===================  ================================================
`Europe/Dublin`      ``nov 14 22:13:20:3``
`UTC`                ``nov 14 22:13:20:3``
`America/New_York`   ``nov 14 17:13:20:3``
===================  ================================================

The first two agree only because Ireland is on UTC in November -- a one-shape
probe would have concluded "not timezone-dependent" and been wrong, which is
why the cases below span both sides of a DST boundary. New York is 5h behind in
November and 4h behind in July, so this is a real timezone database resolved at
the instant; no fixed offset reproduces it.

Every expectation here is a literal mongod answer, and each runs in a
SUBPROCESS with `TZ` set: the rendering depends on the process's zone, so a test
that used the host's would pass in Dublin and fail in CI's UTC.
"""

from __future__ import annotations

import os
import subprocess
import sys

import pytest

bson = pytest.importorskip("bson")

# (TZ, timestamp seconds, increment) -> exactly what mongod 8.2.11 answered.
MEASURED = [
    ("America/New_York", 1, 1, "dec 31 19:00:01:1"),
    ("America/New_York", 1700000000, 3, "nov 14 17:13:20:3"),
    ("America/New_York", 1720000000, 0, "jul  3 05:46:40:0"),
    ("America/New_York", 1767225600, 12, "dec 31 19:00:00:12"),
    ("UTC", 1, 1, "jan  1 00:00:01:1"),
    ("UTC", 1700000000, 3, "nov 14 22:13:20:3"),
    ("UTC", 1720000000, 0, "jul  3 09:46:40:0"),
    ("UTC", 1767225600, 12, "jan  1 00:00:00:12"),
]

_PURE = """
import sys
from bson import Timestamp
from secantus.expressions import evaluate
print(evaluate({"$toLower": Timestamp(int(sys.argv[1]), int(sys.argv[2]))}, {}))
"""

_RUST = """
import sys
import bson
import _secantus_core as rust
from bson import Timestamp
expr = {"e": {"$toLower": Timestamp(int(sys.argv[1]), int(sys.argv[2]))}}
res = rust.evaluate(bson.encode({}), bson.encode(expr), bson.encode({}))
if res is None:
    print("<DEFER>")
else:
    out = bson.decode(res)
    print(out["err"]["errmsg"] if "err" in out else out["r"])
"""


def _run(script: str, tz: str, secs: int, inc: int) -> str:
    env = dict(os.environ, TZ=tz)
    out = subprocess.run(
        [sys.executable, "-c", script, str(secs), str(inc)],
        capture_output=True,
        text=True,
        env=env,
        check=True,
    )
    return out.stdout.strip()


@pytest.mark.parametrize(("tz", "secs", "inc", "expected"), MEASURED)
def test_python_server_matches_mongod(tz: str, secs: int, inc: int, expected: str) -> None:
    assert _run(_PURE, tz, secs, inc) == expected


@pytest.mark.parametrize(("tz", "secs", "inc", "expected"), MEASURED)
def test_rust_server_matches_mongod(tz: str, secs: int, inc: int, expected: str) -> None:
    """The standalone Rust server has no Python to defer to, so it must render.

    This answered `16007 can't convert from BSON type timestamp to String`
    until `secantus-core` gained a way to resolve the local zone -- an operator
    the Python server has always answered.
    """
    pytest.importorskip("_secantus_core")
    assert _run(_RUST, tz, secs, inc) == expected


def test_the_increment_is_not_zero_padded() -> None:
    """`Timestamp(t, 12)` ends `:12`, and `Timestamp(t, 0)` ends `:0`.

    The increment is appended raw, so it is neither width-2 nor dropped when
    zero -- both shapes are in MEASURED above and this names the rule.
    """
    assert _run(_PURE, "UTC", 1767225600, 12).endswith(":12")
    assert _run(_PURE, "UTC", 1720000000, 0).endswith(":0")


def test_the_day_of_month_is_space_padded() -> None:
    """`%e`, not `%d`: July 3rd renders `jul  3`, with TWO spaces before the 3.

    One is the literal separator and one is `%e`'s pad. A `%d` rendering would
    give `jul 03`, which mongod does not produce.
    """
    assert "jul  3 " in _run(_PURE, "UTC", 1720000000, 0)
