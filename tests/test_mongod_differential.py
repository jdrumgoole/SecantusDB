"""Differential conformance: run the same operations against SecantusDB and mongod.

This is the tooling that found nine real bugs during the 2026-08 backlog audit —
three crash-class (an unhandled exception surfacing as "internal server error"),
three that silently wrote or returned wrong data, one where adding an index changed
query results, and two missing capabilities. **None of them came from reading the
backlog**; every one came from executing behaviour and comparing.

It lived in a scratchpad and died with the session. Here it is a standing gate.

How it works: each case is a tiny, independent operation — seed a document, run one
query or update, compare the result. mongod is the oracle. A case that disagrees
names one behaviour, so a failure is actionable rather than "sort is wrong".

Skipped when no `mongod` binary is on PATH, the same way `tests/test_mongosh.py`
and the database-tools tests skip. That keeps the default suite green on machines
and CI lanes without MongoDB installed, while giving real coverage where it exists.
Run explicitly with `pytest -m differential`.
"""

from __future__ import annotations

import shutil
import socket
import subprocess
import tempfile
import time
from collections.abc import Callable, Iterator
from pathlib import Path

import pytest
from bson import Decimal128, Int64, ObjectId
from pymongo import MongoClient
from pymongo.database import Database

from secantus import SecantusDBServer

pytestmark = pytest.mark.differential

MONGOD = shutil.which("mongod")
requires_mongod = pytest.mark.skipif(MONGOD is None, reason="no mongod on PATH")


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


@pytest.fixture(scope="module")
def mongod_uri() -> Iterator[str]:
    """A throwaway standalone mongod. Module-scoped: startup dominates runtime."""
    if MONGOD is None:
        pytest.skip("no mongod on PATH")
    tmp = tempfile.mkdtemp(prefix="differential-mongod-")
    port = _free_port()
    proc = subprocess.Popen(
        [MONGOD, "--port", str(port), "--dbpath", tmp, "--quiet"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    uri = f"mongodb://127.0.0.1:{port}/"
    try:
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            try:
                MongoClient(uri, serverSelectionTimeoutMS=500).admin.command("ping")
                break
            except Exception:  # noqa: BLE001 - polling for readiness
                time.sleep(0.25)
        else:
            pytest.skip("mongod did not become ready")
        yield uri
    finally:
        proc.terminate()
        proc.wait(timeout=30)
        shutil.rmtree(tmp, ignore_errors=True)


@pytest.fixture(scope="module")
def secantus_uri(tmp_path_factory: pytest.TempPathFactory) -> Iterator[str]:
    path: Path = tmp_path_factory.mktemp("differential-secantus")
    with SecantusDBServer(port=0, storage_path=str(path)) as srv:
        yield srv.uri


def _run(uri: str, db_name: str, seed: list[dict], op: Callable[[Database], object]) -> str:
    """Apply one case and return a comparable rendering of its outcome.

    Errors are captured as `code=N`, not swallowed: *which* server rejects an
    operation, and with which code, is as much a part of the contract as what it
    computes — two of the audit's findings were wrong error codes.
    """
    client = MongoClient(uri, serverSelectionTimeoutMS=10000)
    try:
        db = client[db_name]
        db.c.drop()
        if seed:
            db.c.insert_many([dict(d) for d in seed])
        try:
            return repr(op(db))
        except Exception as exc:  # noqa: BLE001 - the error IS the result
            return f"ERROR code={getattr(exc, 'code', None)}"
    finally:
        client.close()


# --------------------------------------------------------------------- cases
# (name, seed documents, operation). Keep each one minimal: a failure should name
# a single behaviour.

NUMS = [
    {"_id": 1, "x": 0},
    {"_id": 2, "x": 5},
    {"_id": 3, "x": Int64(5)},
    {"_id": 4, "x": Decimal128("5.000")},
    {"_id": 5, "x": True},
    {"_id": 6, "x": None},
    {"_id": 7},
]

# 34-significant-digit decimals — the width a 28-digit context silently ate.
DECIMALS_34 = [
    {"_id": 1, "x": Decimal128("1.000000000000000000000000000000001")},
    {"_id": 2, "x": Decimal128("1")},
]

ARRAYS = [
    {"_id": 1, "x": [5, 9]},
    {"_id": 2, "x": [1, 100]},
    {"_id": 3, "x": [7]},
    {"_id": 4, "x": 6},
]


def _err(db: Database, flt: dict, update: dict) -> str:
    """`(code, errmsg)` of a failed update, as a comparable string.

    Comparing the *message* as well as the code is the point: the code was
    already right here, and the message was not.
    """
    from pymongo.errors import OperationFailure, WriteError

    try:
        db.c.update_one(flt, update)
        return "OK"
    except (WriteError, OperationFailure) as exc:
        return f"{exc.code}: {exc.details.get('errmsg')}"


QUERY_CASES: list[tuple[str, list[dict], Callable[[Database], object]]] = [
    ("eq-numeric-unifies-types", NUMS, lambda db: sorted(d["_id"] for d in db.c.find({"x": 5}))),
    ("eq-true-is-not-one", NUMS, lambda db: sorted(d["_id"] for d in db.c.find({"x": 1}))),
    ("eq-null-matches-missing", NUMS, lambda db: sorted(d["_id"] for d in db.c.find({"x": None}))),
    (
        "eq-nan-is-reachable",
        [{"_id": 1, "x": float("nan")}],
        lambda db: db.c.count_documents({"x": float("nan")}),
    ),
    (
        "id-nan-is-reachable-by-key",
        [{"_id": float("nan"), "v": 1}],
        lambda db: db.c.count_documents({"_id": float("nan")}),
    ),
    ("sort-arrays-by-min", ARRAYS, lambda db: [d["_id"] for d in db.c.find().sort("x", 1)]),
    ("sort-arrays-by-max-desc", ARRAYS, lambda db: [d["_id"] for d in db.c.find().sort("x", -1)]),
    (
        "avg-decimal-mixed",
        NUMS,
        lambda db: [
            str(d["a"]) for d in db.c.aggregate([{"$group": {"_id": None, "a": {"$avg": "$x"}}}])
        ],
    ),
    (
        "arrayelemat-out-of-range-is-missing",
        [{"_id": 1, "a": [1, 2]}],
        lambda db: list(db.c.aggregate([{"$project": {"r": {"$arrayElemAt": ["$a", 9]}}}])),
    ),
    # Decimal128 carries 34 significant digits. Comparing `str(...)` rather
    # than the value is deliberate: it catches both lost precision and a lost
    # quantum (`5.00` vs `5`), which compare equal as numbers.
    (
        "sum-decimal-keeps-34-digits",
        DECIMALS_34,
        lambda db: [
            str(d["s"]) for d in db.c.aggregate([{"$group": {"_id": None, "s": {"$sum": "$x"}}}])
        ],
    ),
    (
        "avg-decimal-keeps-34-digits",
        DECIMALS_34,
        lambda db: [
            str(d["s"]) for d in db.c.aggregate([{"$group": {"_id": None, "s": {"$avg": "$x"}}}])
        ],
    ),
    (
        "sum-decimal-preserves-quantum",
        [{"_id": 1, "x": Decimal128("2.50")}, {"_id": 2, "x": Decimal128("0.10")}],
        lambda db: [
            str(d["s"]) for d in db.c.aggregate([{"$group": {"_id": None, "s": {"$sum": "$x"}}}])
        ],
    ),
    (
        "sum-decimal-mixed-with-double",
        [{"_id": 1, "x": Decimal128("1.5")}, {"_id": 2, "x": 3.0}],
        lambda db: [
            str(d["s"]) for d in db.c.aggregate([{"$group": {"_id": None, "s": {"$sum": "$x"}}}])
        ],
    ),
    # mongod uses two *different* double→decimal conversions: the accumulators
    # take the exact binary value, while $inc/$mul/$toDecimal take 15
    # significant digits. These two cases and `inc-decimal-by-double-*` below
    # pin both halves — they answer differently for the same operand.
    (
        "sum-double-uses-the-exact-binary-value",
        [{"_id": 1, "x": Decimal128("0")}, {"_id": 2, "x": 0.1}],
        lambda db: [
            str(d["s"]) for d in db.c.aggregate([{"$group": {"_id": None, "s": {"$sum": "$x"}}}])
        ],
    ),
    (
        "sum-integral-double-keeps-exponent-zero",
        [{"_id": 1, "x": Decimal128("0")}, {"_id": 2, "x": 1e10}],
        lambda db: [
            str(d["s"]) for d in db.c.aggregate([{"$group": {"_id": None, "s": {"$sum": "$x"}}}])
        ],
    ),
    (
        "todecimal-double-uses-15-digits",
        [{"_id": 1, "x": 0.1}],
        lambda db: [
            str(d["r"]) for d in db.c.aggregate([{"$project": {"r": {"$toDecimal": "$x"}}}])
        ],
    ),
    (
        "todecimal-double-terminating-still-pads-to-15",
        [{"_id": 1, "x": 4.125}],
        lambda db: [
            str(d["r"]) for d in db.c.aggregate([{"$project": {"r": {"$toDecimal": "$x"}}}])
        ],
    ),
    (
        "convert-to-decimal-matches-todecimal",
        [{"_id": 1, "x": 4.125}],
        lambda db: [
            str(d["r"])
            for d in db.c.aggregate(
                [{"$project": {"r": {"$convert": {"input": "$x", "to": "decimal"}}}}]
            )
        ],
    ),
]

UPDATE_CASES: list[tuple[str, list[dict], Callable[[Database], object]]] = [
    (
        "inc-on-string-is-type-mismatch",
        [{"_id": 1, "n": "x"}],
        lambda db: db.c.update_one({"_id": 1}, {"$inc": {"n": 1}}) and db.c.find_one({"_id": 1}),
    ),
    (
        "inc-on-bool-is-type-mismatch",
        [{"_id": 1, "n": True}],
        lambda db: db.c.update_one({"_id": 1}, {"$inc": {"n": 1}}) and db.c.find_one({"_id": 1}),
    ),
    (
        "mul-on-string-is-type-mismatch",
        [{"_id": 1, "n": "x"}],
        lambda db: db.c.update_one({"_id": 1}, {"$mul": {"n": 2}}) and db.c.find_one({"_id": 1}),
    ),
    (
        "mul-missing-field-is-zero",
        [{"_id": 1}],
        lambda db: db.c.update_one({"_id": 1}, {"$mul": {"n": 5}}) and db.c.find_one({"_id": 1}),
    ),
    (
        "addtoset-field-order-is-significant",
        [{"_id": 1, "a": [{"x": 1, "y": 2}]}],
        lambda db: (
            db.c.update_one({"_id": 1}, {"$addToSet": {"a": {"y": 2, "x": 1}}})
            and db.c.find_one({"_id": 1})["a"]
        ),
    ),
    (
        "inc-preserves-long",
        [{"_id": 1, "n": Int64(1)}],
        lambda db: (
            db.c.update_one({"_id": 1}, {"$inc": {"n": 1}})
            and type(db.c.find_one({"_id": 1})["n"]).__name__
        ),
    ),
    # The `$inc`/`$mul` type error: mongod puts the *document's `_id`* in the
    # braces, not the field being incremented. We rendered `{n}` where mongod
    # renders `{_id: 1}` — the right code (14) on a message no real server emits.
    (
        "inc-type-error-message-names-the-doc-id",
        [{"_id": 1, "n": "x"}],
        lambda db: _err(db, {"_id": 1}, {"$inc": {"n": 1}}),
    ),
    (
        "inc-type-error-on-null-field",
        [{"_id": 1, "n": None}],
        lambda db: _err(db, {"_id": 1}, {"$inc": {"n": 1}}),
    ),
    (
        "mul-type-error-message-names-the-doc-id",
        [{"_id": 1, "n": "x"}],
        lambda db: _err(db, {"_id": 1}, {"$mul": {"n": 2}}),
    ),
    (
        "inc-type-error-with-objectid-id",
        [{"_id": ObjectId("60a0b0c0d0e0f00102030405"), "n": "x"}],
        lambda db: _err(db, {}, {"$inc": {"n": 1}}),
    ),
    (
        "inc-type-error-dotted-path-names-the-leaf",
        [{"_id": 1, "a": {"b": "x"}}],
        lambda db: _err(db, {"_id": 1}, {"$inc": {"a.b": 1}}),
    ),
    (
        "inc-non-numeric-operand-message",
        [{"_id": 1, "n": 1}],
        lambda db: _err(db, {"_id": 1}, {"$inc": {"n": "x"}}),
    ),
    (
        "min-is-cross-type-not-numeric-only",
        [{"_id": 1, "n": "x"}],
        lambda db: db.c.update_one({"_id": 1}, {"$min": {"n": 5}}) and db.c.find_one({"_id": 1}),
    ),
    (
        "inc-decimal-keeps-34-digits",
        [{"_id": 1, "n": Decimal128("1.000000000000000000000000000000001")}],
        lambda db: (
            db.c.update_one({"_id": 1}, {"$inc": {"n": Decimal128("1")}})
            and str(db.c.find_one({"_id": 1})["n"])
        ),
    ),
    (
        "inc-decimal-preserves-quantum",
        [{"_id": 1, "n": Decimal128("2.50")}],
        lambda db: (
            db.c.update_one({"_id": 1}, {"$inc": {"n": Decimal128("0.10")}})
            and str(db.c.find_one({"_id": 1})["n"])
        ),
    ),
    (
        "mul-decimal-preserves-quantum",
        [{"_id": 1, "n": Decimal128("2.50")}],
        lambda db: (
            db.c.update_one({"_id": 1}, {"$mul": {"n": Decimal128("2")}})
            and str(db.c.find_one({"_id": 1})["n"])
        ),
    ),
    (
        "mul-decimal-rounds-half-even-past-34-digits",
        [{"_id": 1, "n": Decimal128("1.234567890123456789012345678901234")}],
        lambda db: (
            db.c.update_one({"_id": 1}, {"$mul": {"n": Decimal128("9.999")}})
            and str(db.c.find_one({"_id": 1})["n"])
        ),
    ),
    (
        "inc-decimal-by-double-takes-the-double-quantum",
        [{"_id": 1, "n": Decimal128("1.5")}],
        lambda db: (
            db.c.update_one({"_id": 1}, {"$inc": {"n": 3.0}})
            and str(db.c.find_one({"_id": 1})["n"])
        ),
    ),
]

ALL_CASES = [("query", c) for c in QUERY_CASES] + [("update", c) for c in UPDATE_CASES]


@requires_mongod
@pytest.mark.parametrize("kind,case", ALL_CASES, ids=[f"{k}-{c[0]}" for k, c in ALL_CASES])
def test_matches_mongod(kind, case, secantus_uri: str, mongod_uri: str) -> None:
    """SecantusDB must answer exactly what mongod answers."""
    name, seed, op = case
    db_name = f"diff_{kind}_{name.replace('-', '_')}"
    ours = _run(secantus_uri, db_name, seed, op)
    theirs = _run(mongod_uri, db_name, seed, op)
    assert ours == theirs, f"{name}: mongod={theirs} ours={ours}"
