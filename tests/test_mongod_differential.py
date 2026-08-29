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
from collections.abc import Callable, Iterator, Mapping
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


def _agg_err(db: Database, pipeline: list) -> str:
    """`(code, errmsg)` of a failed aggregation, as a comparable string.

    mongod wraps a *runtime* aggregation error in
    ``PlanExecutor error during aggregation :: caused by :: <msg>`` (and a
    constant-folded one in ``Failed to optimize pipeline :: caused by ::``).
    We emit the bare message — a separately tracked gap, since reproducing the
    choice means modelling mongod's constant folding for message text alone.
    Strip the wrapper on both sides so these cases assert the code and the
    message that matter, not the known-missing prefix.
    """
    from pymongo.errors import OperationFailure

    try:
        return f"ok:{len(list(db.c.aggregate(pipeline)))}"
    except OperationFailure as exc:
        msg = str(exc.details.get("errmsg", ""))
        for wrapper in (
            "PlanExecutor error during aggregation :: caused by :: ",
            "Failed to optimize pipeline :: caused by :: ",
        ):
            if msg.startswith(wrapper):
                msg = msg[len(wrapper) :]
        return f"{exc.code}: {msg}"


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
    # A field path that doesn't resolve is MISSING, not null: mongod omits the
    # key. We emitted `z: null` on every document — an extra key mongod never
    # sends, so a client testing `"z" in doc` saw the opposite of the truth.
    (
        "project-missing-path-omits-the-key",
        [{"_id": 1, "n": {"k": 1}}, {"_id": 2, "n": {}}, {"_id": 3}],
        lambda db: list(db.c.aggregate([{"$project": {"z": "$nope"}}, {"$sort": {"_id": 1}}])),
    ),
    (
        "project-missing-nested-path-omits-the-key",
        [{"_id": 1, "n": {"k": 1}}, {"_id": 2, "n": {}}, {"_id": 3}],
        lambda db: list(db.c.aggregate([{"$project": {"z": "$n.k"}}, {"$sort": {"_id": 1}}])),
    ),
    (
        "addfields-missing-path-omits-the-key",
        [{"_id": 1, "a": 1}, {"_id": 2}],
        lambda db: list(db.c.aggregate([{"$addFields": {"z": "$nope"}}, {"$sort": {"_id": 1}}])),
    ),
    (
        "document-literal-drops-missing-member",
        [{"_id": 1, "a": 1}],
        lambda db: list(db.c.aggregate([{"$project": {"z": {"w": "$nope"}}}])),
    ),
    # ...but a missing path is still *null* as an operator argument, which is a
    # different rule and was already right. Pinned so the fix above can't
    # over-reach into operator arguments.
    (
        "missing-path-is-null-as-an-operator-argument",
        [{"_id": 1, "a": 1}],
        lambda db: list(db.c.aggregate([{"$project": {"z": {"$add": ["$nope", 1]}}}])),
    ),
    # $bucket emits a bucket only when something landed in it — boundary
    # buckets and `default` alike. An unused default surfaced as a bare
    # `{_id: "other"}` with no `count`.
    # A `$meta` projection does NOT make the projection inclusion-mode — mongod
    # treats it as a value re-shaper, like $slice. We forced a $meta-only spec
    # into "inclusion of no fields", so asking for a metadata field silently
    # discarded the caller's entire document.
    (
        "meta-projection-keeps-the-whole-document",
        [{"_id": 1, "a": 1, "b": 2}],
        lambda db: list(db.c.find({}, {"m": {"$meta": "indexKey"}})),
    ),
    (
        "meta-projection-honours-id-exclusion",
        [{"_id": 1, "a": 1, "b": 2}],
        lambda db: list(db.c.find({}, {"_id": 0, "m": {"$meta": "indexKey"}})),
    ),
    (
        "meta-alongside-an-inclusion-field",
        [{"_id": 1, "a": 1, "b": 2}],
        lambda db: list(db.c.find({}, {"a": 1, "m": {"$meta": "indexKey"}})),
    ),
    (
        "meta-alongside-an-exclusion-field",
        [{"_id": 1, "a": 1, "b": 2}],
        lambda db: list(db.c.find({}, {"b": 0, "m": {"$meta": "indexKey"}})),
    ),
    # $stdDev* counts only int/long/double/decimal. bool, null, string, array
    # and document values are silently skipped, and mongod ALWAYS emits the
    # field — `null` when the group held no numeric value. Summing every value
    # raised a bare TypeError that escaped as "internal server error" (code 1),
    # and an all-non-numeric group omitted the key entirely.
    (
        "stddev-non-numeric-is-skipped-not-fatal",
        [{"_id": 1, "a": 5}, {"_id": 2, "a": "x"}, {"_id": 3, "a": 7}],
        lambda db: list(db.c.aggregate([{"$group": {"_id": None, "s": {"$stdDevPop": "$a"}}}])),
    ),
    (
        "stddev-no-numeric-value-is-null-not-absent",
        [{"_id": 1, "a": "x"}, {"_id": 2, "a": "y"}],
        lambda db: list(db.c.aggregate([{"$group": {"_id": None, "s": {"$stdDevPop": "$a"}}}])),
    ),
    (
        "stddev-missing-field-is-null-not-absent",
        [{"_id": 1, "b": 1}],
        lambda db: list(db.c.aggregate([{"$group": {"_id": None, "s": {"$stdDevPop": "$a"}}}])),
    ),
    (
        "stddev-bool-is-not-numeric",
        [{"_id": 1, "a": True}, {"_id": 2, "a": False}],
        lambda db: list(db.c.aggregate([{"$group": {"_id": None, "s": {"$stdDevPop": "$a"}}}])),
    ),
    (
        "stddev-decimal-answers-a-double",
        [{"_id": 1, "a": Decimal128("5")}, {"_id": 2, "a": Decimal128("7")}],
        lambda db: list(db.c.aggregate([{"$group": {"_id": None, "s": {"$stdDevPop": "$a"}}}])),
    ),
    (
        "stddev-samp-single-value-is-null",
        [{"_id": 1, "a": 5}],
        lambda db: list(db.c.aggregate([{"$group": {"_id": None, "s": {"$stdDevSamp": "$a"}}}])),
    ),
    # $densify: a doc whose field is null/missing does not participate — mongod
    # emits it unchanged (nulls sort first) and densifies the rest. Sorting the
    # raw list raised a bare TypeError that escaped as "internal server error"
    # (code 1) — a crash where mongod answers.
    (
        "densify-null-field-passes-through",
        [{"_id": 1, "a": 1}, {"_id": 2, "a": None}, {"_id": 3, "a": 5}],
        lambda db: [
            (d.get("_id"), d.get("a"))
            for d in db.c.aggregate(
                [{"$densify": {"field": "a", "range": {"step": 1, "bounds": "full"}}}]
            )
        ],
    ),
    (
        "densify-missing-field-passes-through",
        [{"_id": 1, "a": 1}, {"_id": 2, "x": 9}, {"_id": 3, "a": 5}],
        lambda db: [
            (d.get("_id"), d.get("a"))
            for d in db.c.aggregate(
                [{"$densify": {"field": "a", "range": {"step": 1, "bounds": "full"}}}]
            )
        ],
    ),
    (
        "densify-non-numeric-field-is-rejected",
        [{"_id": 1, "a": 1}, {"_id": 2, "a": "s"}],
        lambda db: _agg_err(
            db, [{"$densify": {"field": "a", "range": {"step": 1, "bounds": "full"}}}]
        ),
    ),
    (
        "densify-all-null-emits-them-unchanged",
        [{"_id": 1, "a": None}, {"_id": 2, "a": None}],
        lambda db: [
            (d.get("_id"), d.get("a"))
            for d in db.c.aggregate(
                [{"$densify": {"field": "a", "range": {"step": 1, "bounds": "full"}}}]
            )
        ],
    ),
    (
        "bucket-omits-the-empty-default",
        [{"_id": 1, "a": 5}, {"_id": 2, "a": 6}],
        lambda db: list(
            db.c.aggregate(
                [{"$bucket": {"groupBy": "$a", "boundaries": [0, 4, 8], "default": "other"}}]
            )
        ),
    ),
    (
        "bucket-omits-an-empty-middle-bucket",
        [{"_id": 1, "a": 1}, {"_id": 2, "a": 7}],
        lambda db: list(
            db.c.aggregate([{"$bucket": {"groupBy": "$a", "boundaries": [0, 2, 4, 8]}}])
        ),
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


def _fam(db: Database, **body: object) -> str:
    """A ``findAndModify`` reply as a comparable string.

    The RAW command, not pymongo's ``find_one_and_*`` wrappers: half of what
    diverged in this sweep was the reply SHAPE -- ``lastErrorObject``'s keys,
    the order of the fields in an upserted document -- which the wrappers hide.
    An upsert-generated ``ObjectId`` differs per server by construction, so it
    is replaced with a marker rather than compared.
    """
    from pymongo.errors import OperationFailure

    cmd: dict = {"findAndModify": "c"}
    cmd.update(body)
    try:
        reply = dict(db.command(cmd))
    except OperationFailure as exc:
        d = exc.details or {}
        return f"{d.get('code')}/{d.get('codeName')}: {d.get('errmsg')!r}"
    out = {k: reply[k] for k in ("lastErrorObject", "value") if k in reply}
    leo = out.get("lastErrorObject")
    if isinstance(leo, dict) and isinstance(leo.get("upserted"), ObjectId):
        out["lastErrorObject"] = {**leo, "upserted": "<oid>"}
    val = out.get("value")
    if isinstance(val, dict) and isinstance(val.get("_id"), ObjectId):
        # Rebuilt rather than updated in place so KEY ORDER is preserved --
        # mongod leads an upserted document with ``_id`` and we did not.
        out["value"] = {"_id": "<oid>", **{k: v for k, v in val.items() if k != "_id"}}
    return repr(out)


def _reply_keys(reply: Mapping) -> list[str]:
    """A reply's field names, minus the cluster-time gossip.

    SecantusDB advertises a replica set, so it attaches ``$clusterTime`` /
    ``operationTime`` to every reply; the standalone ``mongod`` this gate
    spawns does not. That difference is deliberate and is not what these
    cases are about -- the ORDER of the real fields is.
    """
    return [k for k in reply if k not in ("$clusterTime", "operationTime")]


FAM_SEED = [{"_id": 1, "n": 5, "s": "a", "arr": [1, 2, 3], "sub": {"k": 1}}]

# ``findAndModify`` option combinations. A 49-shape probe against mongod
# 6.0.16 found 14 divergences here, two of them silent wrong data: an empty
# update document left every field in place (mongod reduces the document to
# its ``_id``), and an upsert whose query used a dotted path stored a literal
# key with a dot in it. The rest were arguments accepted and ignored, or error
# codes flattened to 14 TypeMismatch on the way out.
FAM_CASES: list[tuple[str, list[dict], Callable[[Database], object]]] = [
    ("empty-update-is-a-replacement", FAM_SEED, lambda db: _fam(db, query={"_id": 1}, update={})),
    (
        "empty-update-leaves-only-the-id",
        FAM_SEED,
        lambda db: (
            (db.command({"findAndModify": "c", "query": {"_id": 1}, "update": {}}))
            and list(db.c.find())
        ),
    ),
    (
        "empty-pipeline-is-a-no-op",
        FAM_SEED,
        lambda db: (
            (db.command({"findAndModify": "c", "query": {"_id": 1}, "update": []}))
            and list(db.c.find())
        ),
    ),
    (
        "upsert-leads-with-id",
        [],
        lambda db: _fam(
            db, query={"b": 1, "a": 2}, update={"$set": {"y": 3}}, upsert=True, new=True
        ),
    ),
    (
        "upsert-nests-a-dotted-query",
        [],
        lambda db: _fam(db, query={"sub.k": 77}, update={"$set": {"y": 1}}, upsert=True, new=True),
    ),
    (
        "upsert-nests-a-deep-dotted-query",
        [],
        lambda db: _fam(db, query={"a.b.c": 5}, update={"$set": {"y": 1}}, upsert=True, new=True),
    ),
    (
        "upsert-merges-dotted-query-and-update",
        [],
        lambda db: _fam(db, query={"a.b": 5}, update={"$set": {"a.c": 1}}, upsert=True, new=True),
    ),
    (
        "upsert-orders-setoninsert-with-set",
        [],
        lambda db: _fam(
            db,
            query={"_id": 42},
            update={"$setOnInsert": {"z": 1}, "$set": {"y": 2}},
            upsert=True,
            new=True,
        ),
    ),
    ("unknown-top-level-field", FAM_SEED, lambda db: _fam(db, query={"_id": 1}, update={}, zz=1)),
    (
        "new-wrong-type",
        FAM_SEED,
        lambda db: _fam(db, query={"_id": 1}, update={"$set": {"n": 1}}, new="yes"),
    ),
    (
        "new-numeric-zero-is-false",
        FAM_SEED,
        lambda db: _fam(db, query={"_id": 1}, update={"$set": {"n": 9}}, new=0),
    ),
    (
        "new-numeric-one-is-true",
        FAM_SEED,
        lambda db: _fam(db, query={"_id": 1}, update={"$set": {"n": 9}}, new=1),
    ),
    (
        "remove-wrong-type",
        FAM_SEED,
        lambda db: _fam(db, query={"_id": 1}, remove="yes"),
    ),
    (
        "arrayfilters-not-an-array",
        FAM_SEED,
        lambda db: _fam(db, query={"_id": 1}, update={"$set": {"n": 1}}, arrayFilters={"e": 1}),
    ),
    (
        "arrayfilters-element-not-a-document",
        FAM_SEED,
        lambda db: _fam(db, query={"_id": 1}, update={"$set": {"n": 1}}, arrayFilters=[5]),
    ),
    (
        "arrayfilters-null",
        FAM_SEED,
        lambda db: _fam(db, query={"_id": 1}, update={"$set": {"n": 1}}, arrayFilters=None),
    ),
    (
        "arrayfilters-unused-identifier",
        FAM_SEED,
        lambda db: _fam(
            db, query={"_id": 1}, update={"$set": {"n": 1}}, arrayFilters=[{"e": {"$gt": 1}}]
        ),
    ),
    (
        "arrayfilters-identifier-with-no-filter",
        FAM_SEED,
        lambda db: _fam(db, query={"_id": 1}, update={"$set": {"arr.$[e]": 1}}),
    ),
    (
        "hint-wrong-type",
        FAM_SEED,
        lambda db: _fam(db, query={"_id": 1}, update={"$set": {"n": 1}}, hint=5),
    ),
    # The error CODES, which all collapsed to 14 TypeMismatch on the way out of
    # findAndModify -- the `update` command had the mapping, this one did not.
    (
        "unknown-modifier-code",
        FAM_SEED,
        lambda db: _fam(db, query={"_id": 1}, update={"$nope": {"n": 1}}),
    ),
    (
        "operator-mixed-with-replacement-field",
        FAM_SEED,
        lambda db: _fam(db, query={"_id": 1}, update={"$set": {"n": 1}, "z": 2}),
    ),
    (
        "immutable-id-code-and-wrapper",
        FAM_SEED,
        lambda db: _fam(db, query={"_id": 1}, update={"$set": {"_id": 9}}),
    ),
    (
        "path-conflict-code",
        FAM_SEED,
        lambda db: _fam(db, query={"_id": 1}, update={"$set": {"a": 2}, "$inc": {"a.b": 1}}),
    ),
    (
        "inc-type-error-is-wrapped",
        [{"_id": 1, "n": "x"}],
        lambda db: _fam(db, query={"_id": 1}, update={"$inc": {"n": 1}}),
    ),
    # PathNotViable: creating through a scalar. This SILENTLY did nothing --
    # the update reported success and wrote no change.
    (
        "create-through-a-scalar",
        FAM_SEED,
        lambda db: _fam(db, query={"_id": 1}, update={"$set": {"n.x": 1}}),
    ),
    (
        "create-through-a-nested-scalar",
        [{"_id": 1, "a": {"b": 7}}],
        lambda db: _fam(db, query={"_id": 1}, update={"$set": {"a.b.c": 1}}),
    ),
    (
        "create-through-an-array-by-name",
        [{"_id": 1, "a": [1]}],
        lambda db: _fam(db, query={"_id": 1}, update={"$set": {"a.x": 9}}),
    ),
    (
        "create-through-an-array-element",
        [{"_id": 1, "a": [1]}],
        lambda db: _fam(db, query={"_id": 1}, update={"$set": {"a.0.x": 9}}),
    ),
    (
        "unset-through-a-scalar-is-allowed",
        FAM_SEED,
        lambda db: _fam(db, query={"_id": 1}, update={"$unset": {"n.x": ""}}, new=True),
    ),
    (
        "out-of-range-index-pads",
        [{"_id": 1, "a": [1]}],
        lambda db: _fam(db, query={"_id": 1}, update={"$set": {"a.4": 9}}, new=True),
    ),
]

# The same two silent-wrong-data rules through the plain ``update`` command,
# which shares the code path -- plus its reply's field order, which puts
# ``upserted`` / ``writeErrors`` BEFORE ``nModified``.
UPDATE_CMD_CASES: list[tuple[str, list[dict], Callable[[Database], object]]] = [
    (
        "cmd-empty-update-is-a-replacement",
        [{"_id": 1, "n": 5, "s": "a"}],
        lambda db: (
            {
                k: v
                for k, v in db.command(
                    {"update": "c", "updates": [{"q": {"_id": 1}, "u": {}}]}
                ).items()
                if k not in ("$clusterTime", "operationTime")
            },
            list(db.c.find()),
        ),
    ),
    (
        "cmd-upsert-nests-a-dotted-query",
        [],
        lambda db: (
            db.command(
                {
                    "update": "c",
                    "updates": [{"q": {"sub.k": 77}, "u": {"$set": {"y": 1}}, "upsert": True}],
                }
            )
            and [{k: v for k, v in d.items() if k != "_id"} for d in db.c.find()]
        ),
    ),
    (
        "cmd-upserted-field-order",
        [],
        lambda db: (
            db.command(
                {
                    "update": "c",
                    "updates": [
                        {"q": {"n": 1, "m": 2}, "u": {"$set": {"z": 3, "a": 4}}, "upsert": True}
                    ],
                }
            )
            and [list(d) for d in db.c.find()]
        ),
    ),
    (
        "cmd-reply-field-order-with-write-errors",
        [{"_id": 1, "n": 5}],
        lambda db: _reply_keys(
            db.command({"update": "c", "updates": [{"q": {"_id": 1}, "u": {"$nope": {"n": 1}}}]})
        ),
    ),
    (
        "cmd-unknown-modifier-message",
        [{"_id": 1, "n": 5}],
        lambda db: db.command(
            {"update": "c", "updates": [{"q": {"_id": 1}, "u": {"$nope": {"n": 1}}}]}
        )["writeErrors"][0],
    ),
    (
        "cmd-path-not-viable",
        [{"_id": 1, "n": 5}],
        lambda db: db.command(
            {"update": "c", "updates": [{"q": {"_id": 1}, "u": {"$set": {"n.x": 1}}}]}
        )["writeErrors"][0],
    ),
]

ALL_CASES = (
    [("query", c) for c in QUERY_CASES]
    + [("update", c) for c in UPDATE_CASES]
    + [("fam", c) for c in FAM_CASES]
    + [("updatecmd", c) for c in UPDATE_CMD_CASES]
)


@requires_mongod
@pytest.mark.parametrize("kind,case", ALL_CASES, ids=[f"{k}-{c[0]}" for k, c in ALL_CASES])
def test_matches_mongod(kind, case, secantus_uri: str, mongod_uri: str) -> None:
    """SecantusDB must answer exactly what mongod answers."""
    name, seed, op = case
    db_name = f"diff_{kind}_{name.replace('-', '_')}"
    ours = _run(secantus_uri, db_name, seed, op)
    theirs = _run(mongod_uri, db_name, seed, op)
    assert ours == theirs, f"{name}: mongod={theirs} ours={ours}"
