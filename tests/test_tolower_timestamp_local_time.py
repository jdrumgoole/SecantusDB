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
#
# `TZ` is a POSIX mechanism and setting it to an IANA name is a Unix-only
# device. Windows' CRT reads `TZ` as `tzn[+|-]hh[:mm[:ss]][dzn]`, so it parses
# "America/New_York" as a zone NAME with no numeric offset -- offset zero -- and
# then enables US daylight rules because a trailing daylight name is present.
# CI measured exactly that on `windows-latest` (2026-09-10): the three winter
# cases came back UTC and the July one came back UTC+1.
#
# That is NOT a wrong answer, which is what this comment used to claim. A real
# mongod 8.2.11 on Windows 11 was finally put next to it (2026-09-18) and gives
# the SAME UTC+1 -- mongod resolves the zone through the same CRT, so the
# platform's `TZ` grammar *is* the server's. What does not travel is the
# EXPECTATIONS below, which are Unix measurements; the behaviour is faithful on
# both. `tests/test_mongod_differential.py::test_timestamp_local_render_matches_mongod`
# asserts that directly against whatever mongod the host has, so it needs no
# platform skip at all -- and it is what caught the Rust engine ignoring `TZ`
# on Windows, which this file could not see.
#
# So the zone-shifting cases are Unix-only. They are the ones that prove the
# rendering is DST-correct rather than a fixed offset, and that claim is about
# the server, not about the platform's `TZ` syntax.
_ZONE_SHIFTED = [
    ("America/New_York", 1, 1, "dec 31 19:00:01:1"),
    ("America/New_York", 1700000000, 3, "nov 14 17:13:20:3"),
    ("America/New_York", 1720000000, 0, "jul  3 05:46:40:0"),
    ("America/New_York", 1767225600, 12, "dec 31 19:00:00:12"),
]

# `TZ=UTC` is spelled the same on both platforms, so these run everywhere.
_UTC = [
    ("UTC", 1, 1, "jan  1 00:00:01:1"),
    ("UTC", 1700000000, 3, "nov 14 22:13:20:3"),
    ("UTC", 1720000000, 0, "jul  3 09:46:40:0"),
    ("UTC", 1767225600, 12, "jan  1 00:00:00:12"),
]

_WINDOWS_TZ = pytest.mark.skipif(
    sys.platform == "win32",
    reason="TZ=<IANA name> is a Unix device; Windows' CRT reads TZ as tzn[+-]hh[dzn]",
)

MEASURED = [*_UTC, *[pytest.param(*case, marks=_WINDOWS_TZ) for case in _ZONE_SHIFTED]]

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
