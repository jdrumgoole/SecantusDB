"""``maxTimeMS`` is validated on every command, the way mongod 8.2.1 does it.

The slot used to be checked in ``find`` alone. ``aggregate``, ``count`` and 21
other commands took a wrong-typed value **silently** -- the worst of the three
failure modes in the wrong-typed-argument sweep, because the caller is told the
operation succeeded. It is a generic command field in mongod's IDL, so it is now
validated once in ``dispatch`` rather than in 24 handlers.

Every expectation below was probed against a live mongod 8.2.1 across 24
commands (436 cases, 0 divergences). Three of the rules would have been got
wrong by reasoning from the 6.0 behaviour the old code implemented:

* **The IDL struct name is the command name for all 24 -- except ``find``,
  which is ``FindCommandRequest``.** So it is a lookup with one entry, not a
  format string, and not "derive it from the command".
* **The check order is load-bearing.** ``-1.5`` is both non-integral and
  negative; mongod answers the integral error (9), not the range error (2).
* **A fractional ``double`` and a fractional ``Decimal128`` get DIFFERENT
  messages** -- "Expected an integer" versus "Cannot represent as a 64-bit
  integer" -- for the same numeric value.

The expected-type list is the same SET on every command, but mongod renders it
in 12 different orders across the 24 -- and reorders it between *patch* builds
(CLAUDE.md). Only the set is asserted here.
"""

from __future__ import annotations

import re
from typing import Any

import pymongo
import pytest
from bson import Decimal128, Int64

from secantus import SecantusDBServer

# One minimal valid body per command that takes maxTimeMS. Not exhaustive of
# mongod's surface -- exhaustive of ours.
COMMAND_BODIES: dict[str, dict[str, Any]] = {
    "find": {"find": "c", "filter": {}},
    "aggregate": {"aggregate": "c", "pipeline": [], "cursor": {}},
    "count": {"count": "c"},
    "distinct": {"distinct": "c", "key": "a"},
    "findAndModify": {"findAndModify": "c", "query": {}, "update": {"$set": {"y": 1}}},
    "insert": {"insert": "c", "documents": [{"z": 1}]},
    "update": {"update": "c", "updates": [{"q": {}, "u": {"$set": {"y": 2}}}]},
    "delete": {"delete": "c", "deletes": [{"q": {"nomatch": 1}, "limit": 1}]},
    "listCollections": {"listCollections": 1},
    "listIndexes": {"listIndexes": "c"},
    "createIndexes": {"createIndexes": "c", "indexes": [{"key": {"q": 1}, "name": "pq"}]},
    "collMod": {"collMod": "c"},
    "dbStats": {"dbStats": 1},
    "explain": {"explain": {"find": "c", "filter": {}}, "verbosity": "queryPlanner"},
    "ping": {"ping": 1},
    "hello": {"hello": 1},
    "buildInfo": {"buildInfo": 1},
}

# mongod's own vocabulary for the slot, as a set -- see the module docstring.
EXPECTED_TYPES = {"decimal", "double", "int", "long"}

TYPES_RE = re.compile(r"expected types '\[([^\]]*)\]'")


@pytest.fixture(scope="module")
def _server(wt_home_module):
    with SecantusDBServer(port=0, storage_path=wt_home_module) as srv:
        yield srv


@pytest.fixture
def db(_server):
    cli = pymongo.MongoClient(
        f"mongodb://{_server.address[0]}:{_server.address[1]}", directConnection=True
    )
    d = cli["maxtimems"]
    d.c.insert_one({"_id": 1, "a": 1})
    try:
        yield d
    finally:
        cli.drop_database(d.name)
        cli.close()


def _err(db, cmd):
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        db.command(dict(cmd))
    assert exc.value.code != 1, f"crashed instead of parsing: {exc.value}"
    return exc.value


def _with(command: str, value: Any) -> dict[str, Any]:
    cmd = dict(COMMAND_BODIES[command])
    cmd["maxTimeMS"] = value
    return cmd


# ---------------------------------------------------------------------------
# The regression this file exists for: every command validates the slot.
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("command", sorted(COMMAND_BODIES))
def test_every_command_rejects_a_wrong_typed_max_time_ms(db, command) -> None:
    """``aggregate`` and ``count`` ACCEPTED a string here and reported success."""
    err = _err(db, _with(command, "x"))
    assert err.code == 14


@pytest.mark.parametrize("command", sorted(COMMAND_BODIES))
def test_struct_name_is_the_command_name_except_for_find(db, command) -> None:
    struct = "FindCommandRequest" if command == "find" else command
    err = _err(db, _with(command, "x"))
    assert err.details["errmsg"].startswith(
        f"BSON field '{struct}.maxTimeMS' is the wrong type 'string', expected types '["
    )


@pytest.mark.parametrize("command", sorted(COMMAND_BODIES))
def test_expected_type_list_is_the_right_set(db, command) -> None:
    """Asserted as a SET: mongod's order is arbitrary per command and changes
    between patch builds, so pinning the order would pin a build."""
    err = _err(db, _with(command, "x"))
    listed = TYPES_RE.search(err.details["errmsg"])
    assert listed is not None, err.details["errmsg"]
    assert {t.strip() for t in listed.group(1).split(",")} == EXPECTED_TYPES


@pytest.mark.parametrize("command", sorted(COMMAND_BODIES))
def test_every_command_rejects_a_negative_max_time_ms(db, command) -> None:
    err = _err(db, _with(command, -1))
    assert err.code == 2
    assert err.details["errmsg"] == ("BSON field 'maxTimeMS' value must be >= 0, actual value '-1'")


@pytest.mark.parametrize("command", sorted(COMMAND_BODIES))
def test_every_command_accepts_an_explicit_null(db, command) -> None:
    db.command(_with(command, None))


# ---------------------------------------------------------------------------
# Type errors.
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("value", "bson_type"),
    [("x", "string"), ({}, "object"), ([1], "array"), (True, "bool")],
)
def test_wrong_types_name_the_type_they_got(db, value, bson_type) -> None:
    err = _err(db, _with("find", value))
    assert err.code == 14
    assert f"is the wrong type '{bson_type}'" in err.details["errmsg"]


def test_a_bool_is_not_read_as_an_int(db) -> None:
    """Python makes ``bool`` a subclass of ``int``; without a guard
    ``maxTimeMS: True`` would be accepted as 1."""
    assert _err(db, _with("find", True)).code == 14


# ---------------------------------------------------------------------------
# The 9 FailedToParse family -- four messages, split by BSON type as well as
# by value.
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("value", "errmsg"),
    [
        (1.5, "Expected an integer: maxTimeMS: 1.5"),
        (-1.5, "Expected an integer: maxTimeMS: -1.5"),
        (-0.5, "Expected an integer: maxTimeMS: -0.5"),
        (3.25, "Expected an integer: maxTimeMS: 3.25"),
        (float("nan"), "Expected an integer, but found NaN in: maxTimeMS: nan"),
        (float("inf"), "Cannot represent as a 64-bit integer: maxTimeMS: inf"),
        (float("-inf"), "Cannot represent as a 64-bit integer: maxTimeMS: -inf"),
        (1e100, "Cannot represent as a 64-bit integer: maxTimeMS: 1e+100"),
        (-1e100, "Cannot represent as a 64-bit integer: maxTimeMS: -1e+100"),
    ],
)
def test_double_integrality_messages(db, value, errmsg) -> None:
    err = _err(db, _with("find", value))
    assert err.code == 9
    assert err.details["errmsg"] == errmsg


@pytest.mark.parametrize(
    ("literal", "errmsg"),
    [
        ("1.5", "Cannot represent as a 64-bit integer: maxTimeMS: 1.5"),
        ("-2.5", "Cannot represent as a 64-bit integer: maxTimeMS: -2.5"),
        ("1E+40", "Cannot represent as a 64-bit integer: maxTimeMS: 1E+40"),
        ("NaN", "Cannot represent as a 64-bit integer: maxTimeMS: NaN"),
        ("Infinity", "Cannot represent as a 64-bit integer: maxTimeMS: Infinity"),
    ],
)
def test_decimal128_integrality_messages(db, literal, errmsg) -> None:
    """A fractional Decimal128 does NOT share the double's wording, and the
    literal the client sent is echoed back verbatim (``1E+40``, not ``1e+40``)."""
    err = _err(db, _with("find", Decimal128(literal)))
    assert err.code == 9
    assert err.details["errmsg"] == errmsg


def test_non_integral_and_negative_answers_the_integral_error(db) -> None:
    """The check ORDER, pinned: -1.5 is both, and mongod answers 9, not 2."""
    err = _err(db, _with("find", -1.5))
    assert err.code == 9


# ---------------------------------------------------------------------------
# The 2 BadValue range family. Note it carries NO struct prefix where the type
# message does -- that asymmetry is mongod's.
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("value", "rendered"),
    [
        (-1, "-1"),
        (-1.0, "-1"),
        (Decimal128("-1"), "-1"),
        (Int64(-5), "-5"),
        (Int64(-(2**63)), "-9223372036854775808"),
    ],
)
def test_negative_values_render_as_integers(db, value, rendered) -> None:
    err = _err(db, _with("find", value))
    assert err.code == 2
    assert err.details["errmsg"] == (
        f"BSON field 'maxTimeMS' value must be >= 0, actual value '{rendered}'"
    )


@pytest.mark.parametrize("value", [2**31, 2147483648.0, Decimal128("2147483648"), Int64(2**62)])
def test_values_above_int32_max_are_out_of_range(db, value) -> None:
    err = _err(db, _with("find", value))
    assert err.code == 2
    assert err.details["errmsg"].startswith(
        "BSON field 'maxTimeMS' value must be <= 2147483647, actual value '"
    )


@pytest.mark.parametrize("value", [0, 1000, 1000.0, Decimal128("1000"), Int64(1000), 2**31 - 1])
def test_valid_values_are_accepted(db, value) -> None:
    db.command(_with("find", value))


# ---------------------------------------------------------------------------
# Where the check sits in dispatch. Both neighbours were settled by probing an
# auth-enabled mongod, because either order was plausible.
# ---------------------------------------------------------------------------


def test_command_not_found_wins_over_a_bad_max_time_ms(db) -> None:
    """59, not 14 -- so the check must stay below the handler lookup."""
    err = _err(db, {"nosuchcommand": 1, "maxTimeMS": "x"})
    assert err.code == 59


def test_get_more_validates_before_the_await_data_rejection(db) -> None:
    """``getMore`` has its own rule for the slot -- it is only legal on an
    awaitData cursor -- and mongod applies the generic type check first."""
    db.c.insert_many([{"a": i} for i in range(5)])
    cursor_id = db.command({"find": "c", "filter": {}, "batchSize": 2})["cursor"]["id"]
    err = _err(db, {"getMore": cursor_id, "collection": "c", "maxTimeMS": "x"})
    assert err.code == 14
    assert err.details["errmsg"].startswith("BSON field 'getMore.maxTimeMS'")


def test_get_more_still_rejects_max_time_ms_on_a_non_await_data_cursor(db) -> None:
    """The command-specific rule survives the generic check being added."""
    db.c.insert_many([{"a": i} for i in range(5)])
    cursor_id = db.command({"find": "c", "filter": {}, "batchSize": 2})["cursor"]["id"]
    err = _err(db, {"getMore": cursor_id, "collection": "c", "maxTimeMS": 1000})
    assert err.code == 2
    assert "non-awaitData cursor" in err.details["errmsg"]
