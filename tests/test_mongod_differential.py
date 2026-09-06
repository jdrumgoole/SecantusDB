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

import re
import shutil
import socket
import subprocess
import tempfile
import time
from collections.abc import Callable, Iterator, Mapping
from datetime import datetime
from pathlib import Path

import pytest
from bson import Binary, Code, Decimal128, Int64, MaxKey, MinKey, ObjectId, Timestamp
from pymongo import MongoClient
from pymongo.database import Database

from secantus import SecantusDBServer

pytestmark = pytest.mark.differential

MONGOD = shutil.which("mongod")
requires_mongod = pytest.mark.skipif(MONGOD is None, reason="no mongod on PATH")


# Whether this gate can say anything at all on the server it found.
#
# Every expectation here is an EXACT match against mongod, and mongod's error
# surface moves between majors, so the gate is only meaningful on the series the
# codebase is probed against. Off that series it skips rather than reporting
# version differences as SecantusDB divergences.
#
# The target was 6.0.16 until 2026-08-29 and is now 8.2.1. What moved, for
# anyone reading an older comment or a 6.0 server:
#
#   * negative cursor sizing   51024 Location51024 -> 2 BadValue
#   * expected-type lists      '[bool, long, int, decimal, double']  (closing
#                              quote INSIDE the bracket, a real 6.0 quirk)
#                              -> properly quoted, and REORDERED PER FIELD
#   * update failures          bare message -> "Plan executor error during
#                              update :: caused by :: " (execution-time only)
#   * aggregate failures       bare message -> "Executor error during aggregate
#                              command on namespace: <ns> :: caused by :: "
#                              (execution-time only -- a parse error stays bare)
#   * null-valued arguments    rejected (10065) -> treated as ABSENT
#   * IDL-parsed surfaces      $lookup and distinct moved to IDL parsing:
#                              hand-written 9 -> 40414 / 40415 / 14, and the
#                              field is named '$lookup.as' / the struct is
#                              'distinctCommandRequest'
#   * IDL code names           Location40414/40415 -> IDLFailedToParse /
#                              IDLUnknownField
#
# WHY THE WHOLE FILE, NOT A LIST OF KNOWN-VARIANT CASES. That was tried first.
# It needs updating by whoever adds a case, and they cannot see the problem: a
# dev box on another series sees only false failures. In one afternoon three
# PRs added cases that failed on the wrong series. Gating the file is
# self-maintaining: a new case needs no thought, and the gate can never claim a
# divergence it cannot actually judge.
#
# The cost is real: on another series this file provides no coverage. That is
# the honest answer -- across majors an exact-match gate has no expectation to
# assert. Within the major it runs, so drift shows up as a failure.
#
# THIS FILE NOW RUNS IN CI. The Linux lanes install `mongodb-org-server` from
# the 8.2 apt repo, so `@requires_mongod` no longer skips there. Until then the
# reasoning above ended "CI installs mongosh and database-tools but NOT mongod,
# so this file skips there entirely" -- which was true, and meant the one gate
# that compares this project against the real server lived only on whichever
# dev box happened to run it. The suite's `addopts` excludes `perf` / `online`
# / `slow` but not `differential`, so it needs no separate step.
# Gate on the MAJOR only. mongod's error surface is stable within a major, so a
# mismatch on 8.0 or 8.4 is far more likely to be a real SecantusDB divergence
# than version drift -- and a loud failure is more useful there than a silent
# skip, which is what a (major, minor) gate gave. The exact server every value
# here was probed against is recorded below; if a future 8.x does move one of
# these surfaces, this gate is what will tell you, and the fix is to re-probe
# rather than to widen the skip.
PROBED_MONGOD_MAJOR = 8
#: The exact server the expectations were taken from. Informational -- the gate
#: compares the major above -- but it is the version to reproduce against.
PROBED_MONGOD_VERSION = "8.2.1"
#: Also verified green, 2026-08-30: 8.2.11, which is what
#: ``brew install mongodb/brew/mongodb-community@8.2`` installs and what is now
#: linked as this box's default ``mongod``. It needed `_sort_type_lists` below --
#: three cases differed only in the ORDER of an expected-type list. Anything in
#: the 8.2 range should pass; if a new patch release fails, re-probe the case
#: rather than widening the skip.
VERIFIED_ALSO = ("8.2.11",)


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
def mongod_version(mongod_uri: str) -> tuple[int, int]:
    """``(major, minor)`` of the mongod this gate actually spawned.

    Read from the running server rather than ``mongod --version`` so it
    describes the process under test, not whatever else is on PATH.
    """
    client = MongoClient(mongod_uri, serverSelectionTimeoutMS=10000)
    try:
        version_array = client.admin.command("buildInfo")["versionArray"]
    finally:
        client.close()
    return int(version_array[0]), int(version_array[1])


@pytest.fixture(scope="module")
def secantus_uri(tmp_path_factory: pytest.TempPathFactory) -> Iterator[str]:
    path: Path = tmp_path_factory.mktemp("differential-secantus")
    with SecantusDBServer(port=0, storage_path=str(path)) as srv:
        yield srv.uri


# mongod renders a wrong-type error's expected-type list in an arbitrary order:
# `[long, int, double, bool, decimal]` on 8.2.11 versus
# `[int, decimal, long, bool, double]` on 8.2.1, for the same field. The order is
# stable per build (checked across separate processes) but changes between patch
# releases, so pinning it pins a build rather than a behaviour -- 8.2.11, the
# version `brew install mongodb-community@8.2` gives, failed three cases against
# expectations probed from 8.2.1. It is a SET; compare it as one. Everything
# else about the message, including the type NAMES, is still asserted exactly.
_TYPE_LIST = re.compile(r"(expected types \\?')\[([^\]]*)\]")


def _sort_type_lists(rendered: str) -> str:
    def repl(m: re.Match) -> str:
        items = sorted(part.strip() for part in m.group(2).split(",") if part.strip())
        return f"{m.group(1)}[{', '.join(items)}]"

    return _TYPE_LIST.sub(repl, rendered)


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
            return _sort_type_lists(repr(op(db)))
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
    These cases strip the wrapper on both sides so they assert the code and the
    message rather than the prefix.

    Do not read that as "the prefix is unreachable" — ``AGGERR_CASES`` below
    asserts the FULL message, wrapper included, for the stages that carry it.
    What is still deferred is modelling mongod's constant folding in general;
    ``$switch``, where folding changes the answer and not just the text, is
    handled (see ``aggregate._fold_constant_switches``).
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


# The 2026-09-06 read-path sweep's corpus, trimmed to the values that actually
# separated the servers from mongod: NaN, the infinities, a Decimal128, a
# BinData pair whose length and bytes disagree about order, and the BSON types
# `$type` accepted as aliases but could never match.
READPATH = [
    {"_id": 1, "x": 0},
    {"_id": 2, "x": 5},
    {"_id": 3, "x": Int64(5)},
    {"_id": 4, "x": Decimal128("5")},
    {"_id": 5, "x": 5.5},
    {"_id": 6, "x": float("nan")},
    {"_id": 7, "x": float("inf")},
    {"_id": 8, "x": float("-inf")},
    {"_id": 9, "x": None},
    {"_id": 10},
    {"_id": 11, "x": [5]},
]

READPATH_TYPES = [
    {"_id": 1, "x": Code("f")},
    {"_id": 2, "x": MinKey()},
    {"_id": 3, "x": MaxKey()},
    {"_id": 4, "x": Timestamp(1, 1)},
    {"_id": 5, "x": 5},
    {"_id": 6, "x": "s"},
]

READPATH_BINARY = [
    {"_id": 1, "x": Binary(b"")},
    {"_id": 2, "x": Binary(b"\x02")},
    {"_id": 3, "x": Binary(b"\x01\x02")},
    {"_id": 4, "x": Binary(b"\x01")},
]


def _ids(db, filt):
    return sorted(d["_id"] for d in db.c.find(filt))


def _sorted_ids(db, spec):
    """Every sort carries an `_id` tiebreak: mongod's order among EQUAL keys is
    storage order, not a rule (three insertion orders gave three answers when
    probed 8.2.11, 2026-09-06), so a tie sequence must never be asserted."""
    return [d["_id"] for d in db.c.find({}).sort([*spec, ("_id", 1)])]


READPATH_CASES: list[tuple[str, list[dict], Callable[[Database], object]]] = [
    # A NaN range bound: mongod's order calls NaN equal to NaN, so an INCLUSIVE
    # bound matches it and a strict one does not. Both servers answered nothing
    # for all four.
    ("nan-gte-matches-nan", READPATH, lambda db: _ids(db, {"x": {"$gte": float("nan")}})),
    ("nan-lte-matches-nan", READPATH, lambda db: _ids(db, {"x": {"$lte": float("nan")}})),
    ("nan-gt-matches-nothing", READPATH, lambda db: _ids(db, {"x": {"$gt": float("nan")}})),
    ("nan-lt-matches-nothing", READPATH, lambda db: _ids(db, {"x": {"$lt": float("nan")}})),
    ("nan-eq-still-reaches-it", READPATH, lambda db: _ids(db, {"x": float("nan")})),
    # ...and an ordinary bound must not pick the NaN up.
    ("nan-not-in-gte-5", READPATH, lambda db: _ids(db, {"x": {"$gte": 5}})),
    ("nan-not-in-lte-5", READPATH, lambda db: _ids(db, {"x": {"$lte": 5}})),
    # Decimal128 against the infinities: the numeric bridge bailed on any
    # non-finite operand, leaving `float > Decimal128` to raise and be swallowed.
    ("dec-gt-picks-up-inf", READPATH, lambda db: _ids(db, {"x": {"$gt": Decimal128("5")}})),
    ("dec-lt-picks-up-neg-inf", READPATH, lambda db: _ids(db, {"x": {"$lt": Decimal128("5")}})),
    ("lt-inf-picks-up-dec", READPATH, lambda db: _ids(db, {"x": {"$lt": float("inf")}})),
    ("gt-neg-inf-picks-up-dec", READPATH, lambda db: _ids(db, {"x": {"$gt": float("-inf")}})),
    # `$all` bridges the numeric types, as `$eq` does.
    ("all-bridges-decimal", READPATH, lambda db: _ids(db, {"x": {"$all": [5]}})),
    # NaN's place in the sort order: below every other number.
    ("nan-sorts-below-numbers", READPATH, lambda db: _sorted_ids(db, [("x", 1)])),
    ("nan-sorts-below-numbers-desc", READPATH, lambda db: _sorted_ids(db, [("x", -1)])),
    # BinData: length first, then bytes.
    ("bindata-sorts-by-length", READPATH_BINARY, lambda db: _sorted_ids(db, [("x", 1)])),
    # `$type` aliases the Rust table accepted but never matched.
    ("type-javascript", READPATH_TYPES, lambda db: _ids(db, {"x": {"$type": "javascript"}})),
    ("type-minkey", READPATH_TYPES, lambda db: _ids(db, {"x": {"$type": "minKey"}})),
    ("type-maxkey", READPATH_TYPES, lambda db: _ids(db, {"x": {"$type": "maxKey"}})),
    ("type-timestamp", READPATH_TYPES, lambda db: _ids(db, {"x": {"$type": "timestamp"}})),
    ("type-code-13", READPATH_TYPES, lambda db: _ids(db, {"x": {"$type": 13}})),
    ("type-code-minus-1", READPATH_TYPES, lambda db: _ids(db, {"x": {"$type": -1}})),
    ("type-code-127", READPATH_TYPES, lambda db: _ids(db, {"x": {"$type": 127}})),
    # ...and the neighbouring behaviour that was already right, so the fix
    # cannot over-reach.
    ("type-int-unchanged", READPATH_TYPES, lambda db: _ids(db, {"x": {"$type": "int"}})),
    ("type-string-unchanged", READPATH_TYPES, lambda db: _ids(db, {"x": {"$type": "string"}})),
    ("type-number-includes-nan", READPATH, lambda db: _ids(db, {"x": {"$type": "number"}})),
]


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
        return f"{d.get('code')}/{_stable_code_name(d)}: {d.get('errmsg')!r}"
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


def _upsert_key_shape(db: Database, **body: object) -> str:
    """An upserted document's key shape, minus what mongod versions disagree on.

    ``_id`` leads on every version -- that is the bug this pins (we appended it
    LAST) -- and the fields the UPDATE added are in field-name order on every
    version too. What differs: 6.0.16 sorts the fields seeded from the query's
    equalities, while the newer server on the Windows runner keeps the query's
    own order. So the leading ``_id`` and the sorted key set are asserted, and
    the seeded group's internal order is not.
    """
    reply = db.command({"findAndModify": "c", **body})
    keys = list(reply["value"])
    return f"first={keys[0]} rest={sorted(keys[1:])}"


def _stable_code_name(details: Mapping) -> str:
    """``codeName``, or a marker when mongod's name for the code is not stable.

    This gate runs against *whatever* ``mongod`` is on PATH, and the lanes do
    not agree: the dev box has 6.0.16, the Windows runner image ships a newer
    server. mongod's NAMED codes (2 BadValue, 9 FailedToParse, 14 TypeMismatch,
    28, 40, 66 …) are stable across those versions, but the high numeric ones
    are exactly the codes that had no symbolic name in 6.0 -- which renders
    them as the fallback ``Location<N>`` -- and acquired one later. 40415 is
    ``Location40415`` on 6.0.16 and ``IDLUnknownField`` on the newer server,
    with the same code and the same message.

    So the *code* and the *message* are asserted, and the name is asserted only
    where it means something. Found by CI: the case passed on macOS and Linux
    and failed on `test-windows`, which was a real version difference and not a
    flake.
    """
    code = details.get("code")
    name = details.get("codeName")
    if isinstance(code, int) and code >= 10000:
        return "<version-dependent>"
    return str(name)


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
    # ``_id`` FIRST is the part that was broken (we appended it last) and the
    # part every mongod agrees on. The relative order of the query-seeded
    # fields is NOT asserted: 6.0.16 sorts them, and the newer server on the
    # Windows runner keeps the query's own order -- probed on both. We ship
    # 6.0's form, as this file does for every other 6.0-vs-newer split, and
    # `tests/test_update_replacement_and_paths.py` pins it against our server.
    (
        "upsert-leads-with-id",
        [],
        lambda db: _upsert_key_shape(
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
    # ``_id`` leads, and the update-added fields are in field-name order, on
    # every mongod. The seeded group's internal order is version-dependent --
    # see `_upsert_key_shape`.
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
            and [(list(d)[0], sorted(list(d)[1:3]), list(d)[3:]) for d in db.c.find()]
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


def _cursor_cmd(db: Database, cmd: dict) -> str:
    """A cursor-command reply, with the parts that cannot be compared removed.

    Cursor ids differ per server by construction, so an id is rendered as its
    BSON type plus whether it is zero (open vs exhausted) -- which is what
    drivers actually assert -- and an id echoed back inside an error message or
    a ``killCursors`` list is replaced with a marker.
    """
    from pymongo.errors import OperationFailure

    try:
        reply = dict(db.command(cmd))
    except OperationFailure as exc:
        d = exc.details or {}
        msg = str(d.get("errmsg", ""))
        # "cursor id 12345 not found" -- the number is per-server.
        msg = re.sub(r"cursor id \d+", "cursor id <id>", msg)
        return f"{d.get('code')}/{_stable_code_name(d)}: {msg!r}"
    out = {}
    for k, v in reply.items():
        if k in ("$clusterTime", "operationTime"):
            continue
        if k == "cursor" and isinstance(v, dict):
            v = {
                ck: (f"{type(cv).__name__}/{'zero' if cv == 0 else 'open'}" if ck == "id" else cv)
                for ck, cv in v.items()
            }
        elif k.startswith("cursors") and isinstance(v, list):
            v = ["<id>" for _ in v]
        out[k] = v
    return repr(out)


CURSOR_SEED = [{"_id": i} for i in range(1, 11)]


def _with_cursor(cmd_fn):
    """Open a cursor with batchSize 2, then run ``cmd_fn(cursor_id)``."""

    def op(db: Database) -> str:
        cid = db.command({"find": "c", "batchSize": 2})["cursor"]["id"]
        return _cursor_cmd(db, cmd_fn(cid))

    return op


# Cursor / getMore / killCursors. 51 shapes probed, 22 diverged -- FOUR of them
# crash-class, where a malformed argument reached a bare ``int()`` and the
# exception escaped as "internal server error" (code 1). Most of the rest were
# arguments accepted and ignored, or ``CursorNotFound`` (43) answered for what
# mongod reports as a parse error before it looks a cursor up.
CURSOR_CASES: list[tuple[str, list[dict], Callable[[Database], object]]] = [
    # The crashes.
    (
        "getmore-id-string",
        CURSOR_SEED,
        lambda db: _cursor_cmd(db, {"getMore": "x", "collection": "c"}),
    ),
    (
        "getmore-batchsize-string",
        CURSOR_SEED,
        _with_cursor(lambda cid: {"getMore": cid, "collection": "c", "batchSize": "x"}),
    ),
    (
        "killcursors-not-an-array",
        CURSOR_SEED,
        lambda db: _cursor_cmd(db, {"killCursors": "c", "cursors": 5}),
    ),
    (
        "killcursors-element-not-a-long",
        CURSOR_SEED,
        lambda db: _cursor_cmd(db, {"killCursors": "c", "cursors": ["x"]}),
    ),
    # Negative sizing values.
    (
        "find-batchsize-negative",
        CURSOR_SEED,
        lambda db: _cursor_cmd(db, {"find": "c", "batchSize": -3}),
    ),
    ("find-limit-negative", CURSOR_SEED, lambda db: _cursor_cmd(db, {"find": "c", "limit": -3})),
    ("find-skip-negative", CURSOR_SEED, lambda db: _cursor_cmd(db, {"find": "c", "skip": -3})),
    (
        "getmore-batchsize-negative",
        CURSOR_SEED,
        _with_cursor(lambda cid: {"getMore": cid, "collection": "c", "batchSize": -1}),
    ),
    (
        "agg-cursor-batchsize-negative",
        CURSOR_SEED,
        lambda db: _cursor_cmd(db, {"aggregate": "c", "pipeline": [], "cursor": {"batchSize": -1}}),
    ),
    # Accepted numeric shapes -- the range check must not narrow the types.
    (
        "find-batchsize-fractional",
        CURSOR_SEED,
        lambda db: _cursor_cmd(db, {"find": "c", "batchSize": 2.5}),
    ),
    (
        "find-batchsize-decimal",
        CURSOR_SEED,
        lambda db: _cursor_cmd(db, {"find": "c", "batchSize": Decimal128("3")}),
    ),
    (
        "find-batchsize-null",
        CURSOR_SEED,
        lambda db: _cursor_cmd(db, {"find": "c", "batchSize": None}),
    ),
    ("find-batchsize-zero", CURSOR_SEED, lambda db: _cursor_cmd(db, {"find": "c", "batchSize": 0})),
    # getMore's required / typed fields.
    (
        "getmore-id-int32",
        CURSOR_SEED,
        lambda db: _cursor_cmd(db, {"getMore": 5, "collection": "c"}),
    ),
    ("getmore-no-collection", CURSOR_SEED, _with_cursor(lambda cid: {"getMore": cid})),
    (
        "getmore-collection-not-a-string",
        CURSOR_SEED,
        _with_cursor(lambda cid: {"getMore": cid, "collection": 5}),
    ),
    (
        "getmore-unknown-field",
        CURSOR_SEED,
        _with_cursor(lambda cid: {"getMore": cid, "collection": "c", "zz": 1}),
    ),
    (
        "getmore-maxtimems-non-awaitdata",
        CURSOR_SEED,
        _with_cursor(lambda cid: {"getMore": cid, "collection": "c", "maxTimeMS": 10}),
    ),
    (
        "getmore-normal",
        CURSOR_SEED,
        _with_cursor(lambda cid: {"getMore": cid, "collection": "c", "batchSize": 3}),
    ),
    # killCursors.
    (
        "killcursors-missing-cursors",
        CURSOR_SEED,
        lambda db: _cursor_cmd(db, {"killCursors": "c"}),
    ),
    (
        "killcursors-null-cursors",
        CURSOR_SEED,
        lambda db: _cursor_cmd(db, {"killCursors": "c", "cursors": None}),
    ),
    (
        "killcursors-null-element",
        CURSOR_SEED,
        lambda db: _cursor_cmd(db, {"killCursors": "c", "cursors": [None]}),
    ),
    (
        "killcursors-unknown-field",
        CURSOR_SEED,
        lambda db: _cursor_cmd(db, {"killCursors": "c", "cursors": [], "zz": 1}),
    ),
    (
        "killcursors-empty",
        CURSOR_SEED,
        lambda db: _cursor_cmd(db, {"killCursors": "c", "cursors": []}),
    ),
    (
        "killcursors-shape",
        CURSOR_SEED,
        _with_cursor(lambda cid: {"killCursors": "c", "cursors": [cid]}),
    ),
    # aggregate's cursor spec.
    (
        "agg-cursor-missing",
        CURSOR_SEED,
        lambda db: _cursor_cmd(db, {"aggregate": "c", "pipeline": []}),
    ),
    (
        "agg-cursor-unknown-key",
        CURSOR_SEED,
        lambda db: _cursor_cmd(db, {"aggregate": "c", "pipeline": [], "cursor": {"zz": 1}}),
    ),
    (
        "agg-cursor-batchsize-zero",
        CURSOR_SEED,
        lambda db: _cursor_cmd(db, {"aggregate": "c", "pipeline": [], "cursor": {"batchSize": 0}}),
    ),
    # awaitData without tailable.
    (
        "awaitdata-without-tailable",
        CURSOR_SEED,
        lambda db: _cursor_cmd(db, {"find": "c", "awaitData": True}),
    ),
]


def _write_hint(db: Database, cmd: dict) -> str:
    """A write command's reply plus the surviving documents.

    The point is the DOCUMENTS: `delete` / `update` used to ignore an
    unresolvable hint and perform the write, where mongod refuses the
    statement. The writeError's message carries mongod's planner dump, which we
    do not reproduce (see tasks/backlog.md), so only its index and code are
    compared -- the behaviour, not the prose.
    """
    reply = dict(db.command(cmd))
    out = {k: v for k, v in reply.items() if k in ("n", "nModified")}
    out["writeErrors"] = [
        {"index": w.get("index"), "code": w.get("code")} for w in reply.get("writeErrors", [])
    ]
    out["docs"] = sorted(d["_id"] for d in db.c.find())
    return repr(out)


def _read_hint(db: Database, cmd: dict) -> str:
    """A read command's outcome under a hint: the VALUES, or the error code.

    Companion to `_write_hint`. mongod renders its whole query plan in the
    errmsg for an unresolvable hint and we name the hint instead (see
    tasks/backlog.md), so the code is compared and the prose is not.
    """
    from pymongo.errors import OperationFailure

    try:
        reply = dict(db.command(cmd))
    except OperationFailure as exc:
        d = exc.details or {}
        return f"error {d.get('code')}/{_stable_code_name(d)}"
    if "values" in reply:
        return repr(sorted(reply["values"], key=repr))
    return repr([d.get("_id") for d in reply.get("cursor", {}).get("firstBatch", [])])


HINT_SEED = [{"_id": i, "a": i} for i in range(1, 6)]


def _with_index(op):
    """Create `a_1` before running `op` -- these cases need a resolvable hint
    to exist so an UNresolvable one is the only variable."""

    def wrapped(db: Database) -> object:
        db.c.create_index([("a", 1)], name="a_1")
        return op(db)

    return wrapped


# Hint honouring and explain's error handling. The find that matters here is a
# WRITE that should not have happened: `delete` / `update` ignored their
# per-statement `hint` and performed the write where mongod refuses the
# statement.
HINT_CASES: list[tuple[str, list[dict], Callable[[Database], object]]] = [
    (
        "delete-unresolvable-hint-does-not-delete",
        HINT_SEED,
        _with_index(
            lambda db: _write_hint(
                db, {"delete": "c", "deletes": [{"q": {}, "limit": 1, "hint": "nope"}]}
            )
        ),
    ),
    (
        "update-unresolvable-hint-does-not-update",
        HINT_SEED,
        _with_index(
            lambda db: _write_hint(
                db,
                {"update": "c", "updates": [{"q": {}, "u": {"$set": {"z": 1}}, "hint": "nope"}]},
            )
        ),
    ),
    (
        "delete-resolvable-hint-still-deletes",
        HINT_SEED,
        _with_index(
            lambda db: _write_hint(
                db, {"delete": "c", "deletes": [{"q": {}, "limit": 1, "hint": "a_1"}]}
            )
        ),
    ),
    (
        "unordered-batch-continues-past-a-bad-hint",
        HINT_SEED,
        _with_index(
            lambda db: _write_hint(
                db,
                {
                    "delete": "c",
                    "ordered": False,
                    "deletes": [
                        {"q": {"_id": 1}, "limit": 1, "hint": "nope"},
                        {"q": {"_id": 2}, "limit": 1},
                    ],
                },
            )
        ),
    ),
    # `$natural` is a DOCUMENT-only hint; the string is a BadValue on every
    # command that takes one. We accepted the string as a documented
    # convenience, so `pymongo`'s `.hint("$natural")` scanned here and errored
    # against a real server -- probed 8.2.11, 2026-08-31.
    (
        "find-natural-hint-as-string",
        HINT_SEED,
        _with_index(lambda db: _read_hint(db, {"find": "c", "filter": {}, "hint": "$natural"})),
    ),
    (
        "count-natural-hint-as-string",
        HINT_SEED,
        _with_index(lambda db: _read_hint(db, {"count": "c", "hint": "$natural"})),
    ),
    (
        "distinct-natural-hint-as-string",
        HINT_SEED,
        _with_index(lambda db: _read_hint(db, {"distinct": "c", "key": "a", "hint": "$natural"})),
    ),
    (
        "delete-natural-hint-as-string",
        HINT_SEED,
        _with_index(
            lambda db: _write_hint(
                db, {"delete": "c", "deletes": [{"q": {}, "limit": 1, "hint": "$natural"}]}
            )
        ),
    ),
    # `distinct` takes a hint and RESOLVES it: a valid one is honoured, one
    # naming no index refuses the command. It was the only hint-bearing command
    # that ignored the field, so a bogus hint returned full results.
    (
        "distinct-hint-valid-name",
        HINT_SEED,
        _with_index(lambda db: _read_hint(db, {"distinct": "c", "key": "a", "hint": "a_1"})),
    ),
    (
        "distinct-hint-valid-key-spec",
        HINT_SEED,
        _with_index(lambda db: _read_hint(db, {"distinct": "c", "key": "a", "hint": {"a": 1}})),
    ),
    (
        "distinct-hint-unresolvable",
        HINT_SEED,
        _with_index(lambda db: _read_hint(db, {"distinct": "c", "key": "a", "hint": "nope"})),
    ),
    # $natural direction.
    (
        "reverse-natural-hint",
        HINT_SEED,
        lambda db: [
            d["_id"]
            for d in db.command({"find": "c", "filter": {}, "hint": {"$natural": -1}})["cursor"][
                "firstBatch"
            ]
        ],
    ),
    (
        "forward-natural-hint",
        HINT_SEED,
        lambda db: [
            d["_id"]
            for d in db.command({"find": "c", "filter": {}, "hint": {"$natural": 1}})["cursor"][
                "firstBatch"
            ]
        ],
    ),
    (
        "sort-beats-reverse-natural",
        HINT_SEED,
        lambda db: [
            d["_id"]
            for d in db.command(
                {"find": "c", "filter": {}, "hint": {"$natural": -1}, "sort": {"a": 1}}
            )["cursor"]["firstBatch"]
        ],
    ),
    # explain's error handling -- it used to FABRICATE a plan for these.
    (
        "explain-unknown-command",
        HINT_SEED,
        lambda db: _cursor_cmd(db, {"explain": {"nosuchcmd": "c"}, "verbosity": "queryPlanner"}),
    ),
    (
        "explain-empty-command",
        HINT_SEED,
        lambda db: _cursor_cmd(db, {"explain": {}, "verbosity": "queryPlanner"}),
    ),
    (
        "explain-non-document",
        HINT_SEED,
        lambda db: _cursor_cmd(db, {"explain": 5, "verbosity": "queryPlanner"}),
    ),
    (
        "explain-verbosity-wrong-type",
        HINT_SEED,
        lambda db: _cursor_cmd(db, {"explain": {"find": "c"}, "verbosity": 5}),
    ),
    (
        "explain-verbosity-bad-enum",
        HINT_SEED,
        lambda db: _cursor_cmd(db, {"explain": {"find": "c"}, "verbosity": "nope"}),
    ),
    # distinct's unknown fields. Probed with an ALWAYS-unknown name rather than
    # `hint`, whose accepted/rejected status differs by mongod version.
    (
        "distinct-unknown-field",
        HINT_SEED,
        lambda db: _cursor_cmd(db, {"distinct": "c", "key": "a", "zz": 1}),
    ),
    (
        "distinct-still-works",
        HINT_SEED,
        lambda db: sorted(db.command({"distinct": "c", "key": "a"})["values"]),
    ),
]

LK_CHAIN = [
    {"_id": 10, "sku": "a", "parent": None},
    {"_id": 11, "sku": "b", "parent": "a"},
    {"_id": 12, "sku": "c", "parent": "b"},
    {"_id": 13, "sku": None, "parent": "c"},
]

_GRAPH = {
    "from": "stock",
    "startWith": "$sku",
    "connectFromField": "parent",
    "connectToField": "sku",
    "as": "chain",
}


def _join(db: Database, stock: list[dict], pipeline: list) -> str:
    """Run a join pipeline over `c` against a seeded `stock` collection.

    The `as` array's ORDER is not compared -- mongod's reflects its internal
    traversal rather than a documented contract, and this campaign has already
    hit two version splits on ordering. The SET is what the joins are about.
    """
    from pymongo.errors import OperationFailure

    db.stock.drop()
    db.stock.insert_many([dict(d) for d in stock])
    try:
        out = list(db.c.aggregate(pipeline))
    except OperationFailure as exc:
        d = exc.details or {}
        msg = str(d.get("errmsg", ""))
        for w in (
            "PlanExecutor error during aggregation :: caused by :: ",
            "Failed to optimize pipeline :: caused by :: ",
        ):
            if msg.startswith(w):
                msg = msg[len(w) :]
        return f"{d.get('code')}: {msg!r}"
    shaped = []
    for doc in sorted(out, key=lambda d: d["_id"]):
        joined = doc.get("chain", doc.get("s", doc.get("a", {}).get("b") if "a" in doc else None))
        ids = sorted(j["_id"] for j in joined) if isinstance(joined, list) else joined
        shaped.append((doc["_id"], ids))
    return repr(shaped)


# $lookup / $graphLookup. 27 shapes probed, 20 diverged -- the worst a
# TRUNCATED traversal: $graphLookup stopped at the first null link, so a
# four-document chain returned one document with no error.
LOOKUP_CASES: list[tuple[str, list[dict], Callable[[Database], object]]] = [
    (
        "graph-null-link-continues",
        [{"_id": 1, "sku": "a"}],
        lambda db: _join(db, LK_CHAIN, [{"$graphLookup": _GRAPH}]),
    ),
    (
        "graph-missing-link-stops",
        [{"_id": 1, "sku": "a"}],
        lambda db: _join(
            db, [{"_id": 10, "sku": "a"}, {"_id": 11, "sku": None}], [{"$graphLookup": _GRAPH}]
        ),
    ),
    (
        "graph-null-link-skips-fieldless-doc",
        [{"_id": 1, "sku": "a"}],
        lambda db: _join(
            db,
            [{"_id": 10, "sku": "a", "parent": None}, {"_id": 11}],
            [{"$graphLookup": _GRAPH}],
        ),
    ),
    (
        "graph-maxdepth",
        [{"_id": 1, "sku": "a"}],
        lambda db: _join(
            db,
            [
                {"_id": 1, "sku": "a", "parent": "b"},
                {"_id": 2, "sku": "b", "parent": "c"},
                {"_id": 3, "sku": "c", "parent": "d"},
            ],
            [{"$graphLookup": {**_GRAPH, "maxDepth": 1}}],
        ),
    ),
    (
        "graph-negative-maxdepth",
        [{"_id": 1, "sku": "a"}],
        lambda db: _join(db, LK_CHAIN, [{"$graphLookup": {**_GRAPH, "maxDepth": -1}}]),
    ),
    (
        "graph-unknown-argument",
        [{"_id": 1, "sku": "a"}],
        lambda db: _join(db, LK_CHAIN, [{"$graphLookup": {**_GRAPH, "zz": 1}}]),
    ),
    (
        "graph-spec-not-a-document",
        [{"_id": 1, "sku": "a"}],
        lambda db: _join(db, LK_CHAIN, [{"$graphLookup": 5}]),
    ),
    (
        "lookup-empty-array-matches-null",
        [{"_id": 1, "tags": []}],
        lambda db: _join(
            db,
            [{"_id": 10, "sku": "a"}, {"_id": 11, "sku": None}],
            [
                {
                    "$lookup": {
                        "from": "stock",
                        "localField": "tags",
                        "foreignField": "sku",
                        "as": "s",
                    }
                }
            ],
        ),
    ),
    (
        "lookup-dotted-as-nests",
        [{"_id": 1, "sku": "a"}],
        lambda db: _join(
            db,
            [{"_id": 10, "sku": "a"}],
            [
                {
                    "$lookup": {
                        "from": "stock",
                        "localField": "sku",
                        "foreignField": "sku",
                        "as": "a.b",
                    }
                }
            ],
        ),
    ),
    (
        "lookup-let-wrong-type",
        [{"_id": 1, "sku": "a"}],
        lambda db: _join(
            db,
            [{"_id": 10, "sku": "a"}],
            [{"$lookup": {"from": "stock", "let": 5, "pipeline": [], "as": "s"}}],
        ),
    ),
    (
        "lookup-pipeline-wrong-type",
        [{"_id": 1, "sku": "a"}],
        lambda db: _join(
            db,
            [{"_id": 10, "sku": "a"}],
            [{"$lookup": {"from": "stock", "pipeline": 5, "as": "s"}}],
        ),
    ),
    (
        "lookup-unknown-argument",
        [{"_id": 1, "sku": "a"}],
        lambda db: _join(
            db,
            [{"_id": 10, "sku": "a"}],
            [
                {
                    "$lookup": {
                        "from": "stock",
                        "localField": "sku",
                        "foreignField": "sku",
                        "as": "s",
                        "zz": 1,
                    }
                }
            ],
        ),
    ),
    (
        "lookup-missing-as",
        [{"_id": 1, "sku": "a"}],
        lambda db: _join(
            db,
            [{"_id": 10, "sku": "a"}],
            [{"$lookup": {"from": "stock", "localField": "sku", "foreignField": "sku"}}],
        ),
    ),
    (
        "lookup-half-specified-field-pair",
        [{"_id": 1, "sku": "a"}],
        lambda db: _join(
            db,
            [{"_id": 10, "sku": "a"}],
            [{"$lookup": {"from": "stock", "localField": "sku", "as": "s"}}],
        ),
    ),
]


def _plain_cmd(db: Database, cmd: dict) -> str:
    """A command reply reduced to what is comparable across the two servers."""
    from pymongo.errors import OperationFailure

    try:
        db.command(cmd)
    except OperationFailure as exc:
        d = exc.details or {}
        return f"{d.get('code')}/{_stable_code_name(d)}: {str(d.get('errmsg', ''))!r}"
    return "ok"


# ``maxTimeMS``, which is a GENERIC command field -- mongod validates it on
# every command, and we used to check it on ``find`` alone, so 23 commands took
# a wrong-typed value silently. The values here are deliberately either invalid
# or large: a small VALID value (2, 5) makes mongod actually time the operation
# out and answer 50 MaxTimeMSExpired, which we do not enforce -- a real but
# separate gap (backlog §5), and a race against elapsed time that would make
# this gate flaky.
MAXTIME_SEED = [{"_id": 1, "a": 1}]

_MAXTIME_BODIES: dict[str, dict] = {
    "find": {"find": "c", "filter": {}},
    "aggregate": {"aggregate": "c", "pipeline": [], "cursor": {}},
    "count": {"count": "c"},
    "distinct": {"distinct": "c", "key": "a"},
    "insert": {"insert": "c", "documents": [{"z": 1}]},
    "listCollections": {"listCollections": 1},
    "ping": {"ping": 1},
}

_MAXTIME_VALUES: list[tuple[str, object]] = [
    ("string", "x"),
    ("object", {}),
    ("array", [1]),
    ("bool", True),
    ("frac", 1.5),
    ("frac-negative", -1.5),
    ("nan", float("nan")),
    ("inf", float("inf")),
    ("huge-double", 1e100),
    ("dec-frac", Decimal128("1.5")),
    ("dec-nan", Decimal128("NaN")),
    ("negative", -1),
    ("over-int32", 2**31),
    ("null", None),
    ("valid", 1000),
]


def _maxtime_op(body: dict, value: object) -> Callable[[Database], object]:
    """Bind the loop variables -- a lambda in the comprehension would capture
    the last iteration's values for all 105 cases."""

    def op(db: Database) -> str:
        return _plain_cmd(db, {**body, "maxTimeMS": value})

    return op


MAXTIME_CASES: list[tuple[str, list[dict], Callable[[Database], object]]] = [
    (f"maxtimems-{cname}-{vname}", MAXTIME_SEED, _maxtime_op(body, value))
    for cname, body in _MAXTIME_BODIES.items()
    for vname, value in _MAXTIME_VALUES
] + [
    # The null-means-absent rule next door, and its wrong-typed neighbour.
    (
        "createindexes-indexes-null",
        MAXTIME_SEED,
        lambda db: _plain_cmd(db, {"createIndexes": "c", "indexes": None}),
    ),
    (
        "createindexes-indexes-missing",
        MAXTIME_SEED,
        lambda db: _plain_cmd(db, {"createIndexes": "c"}),
    ),
    (
        "createindexes-indexes-int",
        MAXTIME_SEED,
        lambda db: _plain_cmd(db, {"createIndexes": "c", "indexes": 5}),
    ),
]


def _agg_err_full(db: Database, pipeline: list) -> str:
    """Like ``_agg_err``, but keeps mongod's wrapper prefix.

    The namespace inside the executor prefix names the case's own database, so
    both servers render the same string only after it is normalised away.
    """
    from pymongo.errors import OperationFailure

    try:
        return f"ok:{len(list(db.c.aggregate(pipeline)))}"
    except OperationFailure as exc:
        msg = str(exc.details.get("errmsg", ""))
        msg = re.sub(r"namespace: [^ ]+", "namespace: <ns>", msg)
        return f"{exc.code}/{_stable_code_name(exc.details)}: {msg}"


def _rr(new_root: object) -> Callable[[Database], object]:
    return lambda db: _agg_err_full(db, [{"$replaceRoot": {"newRoot": new_root}}])


def _rw(expr: object) -> Callable[[Database], object]:
    return lambda db: _agg_err_full(db, [{"$replaceWith": expr}])


def _redact(spec: object) -> Callable[[Database], object]:
    return lambda db: _agg_err_full(db, [{"$redact": spec}])


def _redact_ok(spec: object) -> Callable[[Database], object]:
    """A `$redact` that SUCCEEDS — the result documents are the assertion."""
    return lambda db: list(db.c.aggregate([{"$redact": spec}]))


def _switch(case: object, **extra: object) -> Callable[[Database], object]:
    body: dict = {"branches": [{"case": case, "then": 1}], **extra}
    return lambda db: _agg_err_full(db, [{"$addFields": {"x": {"$switch": body}}}])


ONE = [{"_id": 1, "n": 1}]

# `$replaceRoot`/`$replaceWith` runtime failure (40228), `$switch` with no
# matching branch (40066 at execution, 40069 when mongod folds it away), and
# `$bucket`'s out-of-range value (7158303). All probed against mongod 8.2.11.
#
# The `Input document:` half of the 40228 message is mongod's DEPENDENCY-PRUNED
# document, not the stored one, so several cases exist only to pin that: which
# fields survive, in which order, and how a dotted or absent path behaves.
AGGERR_CASES: list[tuple[str, list[dict], Callable[[Database], object]]] = [
    ("rr-int", ONE, _rr("$n")),
    ("rr-string", [{"_id": 1, "n": "abc"}], _rr("$n")),
    ("rr-double", [{"_id": 1, "n": 1.5}], _rr("$n")),
    ("rr-long", [{"_id": 1, "n": Int64(2**40)}], _rr("$n")),
    ("rr-decimal", [{"_id": 1, "n": Decimal128("1.25")}], _rr("$n")),
    ("rr-bool", [{"_id": 1, "n": True}], _rr("$n")),
    ("rr-array", [{"_id": 1, "n": [1, {"a": 2}]}], _rr("$n")),
    ("rr-objectid", [{"_id": ObjectId("0123456789ab0123456789ab"), "n": 1}], _rr("$_id")),
    ("rr-date", [{"_id": 1, "n": datetime(2026, 8, 31, 12, 0, 0)}], _rr("$n")),
    ("rr-null-literal", ONE, _rr({"$literal": None})),
    # Pruning: unreferenced fields are dropped, and `_id` is not automatic.
    ("rr-prunes-unreferenced", [{"_id": 1, "n": 1, "s": "hi", "arr": [1, 2]}], _rr("$n")),
    ("rr-keeps-referenced-id", [{"_id": 7, "n": 1}], _rr({"$add": ["$_id", "$n"]})),
    ("rr-no-field-refs", [{"_id": 1}], _rr({"$literal": 5})),
    ("rr-absent-field", ONE, _rr({"$ifNull": ["$missing", 5]})),
    ("rr-absent-parent", ONE, _rr({"$ifNull": ["$a.b", 5]})),
    ("rr-dotted-path", [{"_id": 1, "a": {"b": 5, "c": 6}}], _rr("$a.b")),
    (
        "rr-parent-subsumes-child",
        [{"_id": 1, "a": {"b": 1, "c": 2}}],
        _rr({"$concat": [{"$toString": "$a.b"}, {"$toString": {"$type": "$a"}}]}),
    ),
    ("rr-root-dot-field", [{"_id": 1, "a": 1, "b": 2}], _rr({"$toString": "$$ROOT.a"})),
    # Order follows the DOCUMENT, not the order the expression mentions fields.
    (
        "rr-doc-order",
        [{"_id": 1, "a": 1, "b": 2}],
        _rr({"$concat": [{"$toString": "$b"}, {"$toString": "$a"}]}),
    ),
    (
        "rr-doc-order-nested",
        [{"_id": 1, "a": {"b": 1, "c": 2, "d": 3}}],
        _rr({"$concat": [{"$toString": "$a.c"}, {"$toString": "$a.b"}]}),
    ),
    # `$replaceWith` names a different subject in the same message.
    ("rw-scalar", ONE, _rw("$n")),
    ("rw-literal", ONE, _rw({"$literal": 5})),
    # `$switch`: folded at parse time (40069) vs discovered per document (40066).
    ("switch-field-ref", ONE, _switch({"$gt": ["$n", 99]})),
    ("switch-literal-false", ONE, _switch(False)),
    ("switch-literal-all-const", ONE, _switch({"$gt": [1, 99]})),
    ("switch-with-default", ONE, _switch(False, default=9)),
    # The fold fires with nothing to execute over -- an empty seed is the point.
    ("switch-folds-on-empty", [], _switch(False)),
    ("switch-field-ref-on-empty", [], _switch({"$gt": ["$n", 99]})),
    # `$bucket` reuses mongod's `$switch` text and takes the executor wrapper.
    (
        "bucket-out-of-range",
        [{"_id": 1, "n": 99}],
        lambda db: _agg_err_full(db, [{"$bucket": {"groupBy": "$n", "boundaries": [0, 2, 4]}}]),
    ),
    (
        "bucket-with-default",
        [{"_id": 1, "n": 99}],
        lambda db: _agg_err_full(
            db, [{"$bucket": {"groupBy": "$n", "boundaries": [0, 2, 4], "default": "other"}}]
        ),
    ),
]


# `$redact` — three defects found on 2026-08-31, all of which made the stage
# return data it exists to withhold, and all of which were present on BOTH
# servers. The decision names are VARIABLES bound only inside `$redact`, not the
# strings that spell them: a stored `"$$KEEP"` used to be accepted as a decision
# (disclosure driven by document content), the descent skipped NESTED arrays
# (a tagged sub-doc one array deeper was returned), and the names resolved to
# marker strings anywhere in a pipeline. The 17053 rendering is mongod's compact
# `Value::toString` — a different renderer from the shell form other messages
# use, which is why the container cases are here. Probed on mongod 8.2.11.
LEVELLED = [{"_id": 1, "lvl": 1, "n": [[{"lvl": 9, "x": 1}], {"lvl": 1, "y": 2}], "s": "t"}]
DESCEND_EXPR = {"$cond": [{"$lte": [{"$ifNull": ["$lvl", 0]}, 3]}, "$$DESCEND", "$$PRUNE"]}

REDACT_CASES: list[tuple[str, list[dict], Callable[[Database], object]]] = [
    # A stored string must not impersonate a decision.
    ("stored-keep-string", [{"_id": 1, "tag": "$$KEEP", "s": "x"}], _redact("$tag")),
    ("stored-prune-string", [{"_id": 1, "tag": "$$PRUNE"}], _redact("$tag")),
    ("literal-keep-string", ONE, _redact({"$literal": "$$KEEP"})),
    # The decision names are undefined OUTSIDE $redact.
    ("keep-in-project", ONE, lambda db: _agg_err_full(db, [{"$project": {"x": "$$KEEP"}}])),
    # Descent reaches nested arrays.
    ("nested-arrays", LEVELLED, _redact_ok(DESCEND_EXPR)),
    ("deep-nested-arrays", [{"_id": 1, "lvl": 1, "n": [[[{"lvl": 9}]]]}], _redact_ok(DESCEND_EXPR)),
    # A non-decision result: mongod's code, wrapper, and compact rendering.
    ("non-decision-int", ONE, _redact(5)),
    ("non-decision-string", ONE, _redact({"$literal": "x"})),
    ("non-decision-bool", ONE, _redact({"$literal": True})),
    ("non-decision-null", ONE, _redact(None)),
    ("non-decision-empty-doc", ONE, _redact({})),
    ("non-decision-doc", ONE, _redact({"$literal": {"k": 1, "j": "s"}})),
    ("non-decision-array", ONE, _redact({"$literal": [1, "a"]})),
    ("non-decision-nested", ONE, _redact({"$literal": [[1]]})),
    ("non-decision-double", ONE, _redact({"$literal": 1.5})),
    ("non-decision-decimal", ONE, _redact({"$literal": Decimal128("2")})),
    ("non-decision-oid", ONE, _redact({"$literal": ObjectId("0123456789ab0123456789ab")})),
    ("non-decision-date", ONE, _redact({"$literal": datetime(2026, 1, 2, 3, 4, 5)})),
    ("non-decision-field", ONE, _redact("$n")),
    # Happy paths, so a future fix cannot trade the errors for wrong data.
    ("keep-top", ONE, _redact_ok("$$KEEP")),
    ("descend-classic", LEVELLED, _redact_ok(DESCEND_EXPR)),
]

# Undefined `$$variable` — mongod reports it at PARSE time, so it fires on an
# EMPTY collection where nothing is ever evaluated. Both servers used to answer
# ok:1 there, and the Rust server answered a generic BadValue (2) even with
# documents present. The wrapper is per stage: `Invalid $<stage> :: caused by ::`
# for `$project` / `$addFields` / `$set`, bare everywhere else. The `ok-` cases
# are the false-positive guards: a conservative checker must leave a literal
# `"$$NOPE"` in a `$match` filter, a `$literal`, and every binding form alone.
# Probed on mongod 8.2.11.
#: An empty seed leaves the collection NON-EXISTENT, which is the strongest form
#: of the parse-time check: mongod still reports the undefined variable there.
NONE_AT_ALL: list[dict] = []


def _agg(pipeline: list[dict]) -> Callable[[Database], object]:
    return lambda db: _agg_err_full(db, pipeline)


UNDEFVAR_CASES: list[tuple[str, list[dict], Callable[[Database], object]]] = [
    ("project", ONE, _agg([{"$project": {"x": "$$NOPE"}}])),
    ("addfields", ONE, _agg([{"$addFields": {"x": "$$NOPE"}}])),
    ("set", ONE, _agg([{"$set": {"x": "$$NOPE"}}])),
    ("group", ONE, _agg([{"$group": {"_id": "$$NOPE"}}])),
    ("redact", ONE, _agg([{"$redact": "$$NOPE"}])),
    ("replacewith", ONE, _agg([{"$replaceWith": {"k": "$$NOPE"}}])),
    ("replaceroot", ONE, _agg([{"$replaceRoot": {"newRoot": {"x": "$$NOPE"}}}])),
    ("match-expr", ONE, _agg([{"$match": {"$expr": {"$eq": ["$$NOPE", 1]}}}])),
    ("facet-inner", ONE, _agg([{"$facet": {"f": [{"$project": {"x": "$$NOPE"}}]}}])),
    ("nested-doc", ONE, _agg([{"$addFields": {"x": {"y": "$$NOPE"}}}])),
    ("nested-array", ONE, _agg([{"$addFields": {"x": [1, "$$NOPE"]}}])),
    ("deep-expr", ONE, _agg([{"$set": {"x": {"$concatArrays": [["$$NOPE"]]}}}])),
    # `$let` bindings cannot see each other, and do not escape the `in`.
    (
        "let-escapes",
        ONE,
        _agg([{"$project": {"y": {"$let": {"vars": {"v": 1}, "in": "$$v"}}, "z": "$$v"}}]),
    ),
    (
        "let-siblings",
        ONE,
        _agg([{"$project": {"x": {"$let": {"vars": {"a1": 1, "b1": "$$a1"}, "in": "$$b1"}}}}]),
    ),
    # PARSE time: an empty collection still errors.
    ("empty-collection", NONE_AT_ALL, _agg([{"$project": {"x": "$$NOPE"}}])),
    ("empty-collection-bare", NONE_AT_ALL, _agg([{"$group": {"_id": "$$NOPE"}}])),
    # --- false-positive guards: all of these are VALID -----------------------
    ("ok-match-literal", [{"_id": 1, "s": "$$NOPE"}], _agg([{"$match": {"s": "$$NOPE"}}])),
    ("ok-literal", ONE, _agg([{"$project": {"x": {"$literal": "$$NOPE"}}}])),
    ("ok-let", ONE, _agg([{"$project": {"x": {"$let": {"vars": {"v": 1}, "in": "$$v"}}}}])),
    (
        "ok-let-nested",
        ONE,
        _agg(
            [
                {
                    "$project": {
                        "x": {
                            "$let": {
                                "vars": {"v": 1},
                                "in": {
                                    "$let": {"vars": {"w": "$$v"}, "in": {"$add": ["$$v", "$$w"]}}
                                },
                            }
                        }
                    }
                }
            ]
        ),
    ),
    (
        "ok-map-as",
        ONE,
        _agg([{"$project": {"x": {"$map": {"input": [1, 2], "as": "m", "in": "$$m"}}}}]),
    ),
    (
        "ok-map-default",
        ONE,
        _agg([{"$project": {"x": {"$map": {"input": [1, 2], "in": "$$this"}}}}]),
    ),
    (
        "ok-filter-default",
        ONE,
        _agg([{"$project": {"x": {"$filter": {"input": [1, 2], "cond": {"$gt": ["$$this", 1]}}}}}]),
    ),
    (
        "ok-reduce",
        ONE,
        _agg(
            [
                {
                    "$project": {
                        "x": {
                            "$reduce": {
                                "input": [1, 2],
                                "initialValue": 0,
                                "in": {"$add": ["$$value", "$$this"]},
                            }
                        }
                    }
                }
            ]
        ),
    ),
    ("ok-root", ONE, _agg([{"$project": {"x": "$$ROOT"}}])),
    ("ok-now-type", ONE, _agg([{"$project": {"x": {"$type": "$$NOW"}}}])),
    ("ok-redact-decisions", ONE, _agg([{"$redact": {"$cond": [True, "$$KEEP", "$$PRUNE"]}}])),
    (
        "ok-map-over-let",
        ONE,
        _agg(
            [
                {
                    "$project": {
                        "x": {
                            "$let": {
                                "vars": {"v": 1},
                                "in": {
                                    "$map": {
                                        "input": [1],
                                        "as": "m",
                                        "in": {"$add": ["$$v", "$$m"]},
                                    }
                                },
                            }
                        }
                    }
                }
            ]
        ),
    ),
    (
        "ok-dotted-var",
        ONE,
        _agg([{"$project": {"x": {"$let": {"vars": {"v": {"k": 1}}, "in": "$$v.k"}}}}]),
    ),
]

# `$$REMOVE` IS the missing value — probed 9-for-9 against the equivalent absent
# field path, in every position. It used to be a marker of its own, which leaked:
# `{arr: [1, "$$REMOVE", 2]}` reached `bson.encode` and CRASHED the command,
# `$type` answered "object", `$concat` raised 16702, and the Rust engine deferred
# the variable entirely (a generic BadValue on a server with no Python). The
# `twin-` cases pair each shape with its absent-path equivalent: the two must
# stay identical, which is the whole claim being pinned.
TWO_FIELDS = [{"_id": 1, "a": 1, "b": 2}]


def _af(spec: dict) -> Callable[[Database], object]:
    return lambda db: _agg_err_full(db, [{"$addFields": spec}])


REMOVE_CASES: list[tuple[str, list[dict], Callable[[Database], object]]] = [
    ("project", TWO_FIELDS, lambda db: _agg_err_full(db, [{"$project": {"x": "$$REMOVE"}}])),
    (
        "project-keeps-others",
        TWO_FIELDS,
        lambda db: _agg_err_full(db, [{"$project": {"a": 1, "x": "$$REMOVE"}}]),
    ),
    ("addfields", TWO_FIELDS, _af({"x": "$$REMOVE"})),
    ("set-existing", TWO_FIELDS, _af({"a": "$$REMOVE"})),
    (
        "replacewith",
        TWO_FIELDS,
        lambda db: _agg_err_full(db, [{"$replaceWith": {"k": "$$REMOVE", "j": 1}}]),
    ),
    ("group-id", TWO_FIELDS, lambda db: _agg_err_full(db, [{"$group": {"_id": "$$REMOVE"}}])),
    (
        "group-acc",
        TWO_FIELDS,
        lambda db: _agg_err_full(db, [{"$group": {"_id": None, "s": {"$sum": "$$REMOVE"}}}]),
    ),
    ("nested-doc", TWO_FIELDS, _af({"o": {"p": "$$REMOVE", "q": 1}})),
    # Crashed the Python server: the marker reached bson.encode.
    ("in-array", TWO_FIELDS, _af({"arr": [1, "$$REMOVE", 2]})),
    (
        "let-binding",
        TWO_FIELDS,
        lambda db: _agg_err_full(
            db, [{"$project": {"x": {"$let": {"vars": {"v": "$$REMOVE"}, "in": "$$v"}}}}]
        ),
    ),
    ("ifnull", TWO_FIELDS, _af({"x": {"$ifNull": ["$$REMOVE", 9]}})),
    ("eq-null", TWO_FIELDS, _af({"x": {"$eq": ["$$REMOVE", None]}})),
    ("type", TWO_FIELDS, _af({"x": {"$type": "$$REMOVE"}})),
    ("concat", TWO_FIELDS, _af({"x": {"$concat": ["s", "$$REMOVE"]}})),
    ("sum-pair", TWO_FIELDS, _af({"x": {"$sum": ["$$REMOVE", 1]}})),
    # `$setField` removes for `$$REMOVE` AND for an absent path; only an
    # explicit null writes a null. The null form was rejected outright.
    (
        "setfield-remove",
        TWO_FIELDS,
        _af({"d": {"$setField": {"field": "a", "input": "$$ROOT", "value": "$$REMOVE"}}}),
    ),
    (
        "setfield-absent",
        TWO_FIELDS,
        _af({"d": {"$setField": {"field": "a", "input": "$$ROOT", "value": "$nosuch"}}}),
    ),
    (
        "setfield-null",
        TWO_FIELDS,
        _af({"d": {"$setField": {"field": "a", "input": "$$ROOT", "value": None}}}),
    ),
    # The absent-path twins: each must answer exactly what its `$$REMOVE` pair does.
    ("twin-array", TWO_FIELDS, _af({"arr": [1, "$nosuch", 2]})),
    ("twin-type", TWO_FIELDS, _af({"x": {"$type": "$nosuch"}})),
    ("twin-ifnull", TWO_FIELDS, _af({"x": {"$ifNull": ["$nosuch", 9]}})),
    ("twin-concat", TWO_FIELDS, _af({"x": {"$concat": ["s", "$nosuch"]}})),
    ("twin-nested-doc", TWO_FIELDS, _af({"o": {"p": "$nosuch", "q": 1}})),
]

# "Missing" propagates through the operators whose result IS one of their
# sub-expressions -- `$cond`, `$switch`, `$let`, `$ifNull` -- and does NOT
# through the ones that compute a value (`$add`, `$concat`), which is why both
# families are here. mongod omits the field when a field-value position selects
# a missing sub-expression; both servers wrote a null. It matters in operator
# position too: `{$eq: [{$cond: [true, "$nosuch", 1]}, null]}` is FALSE on
# mongod, because the result is missing rather than null. Probed on 8.2.11.
ONE_A = [{"_id": 1, "a": 1}]


def _z(spec: object) -> Callable[[Database], object]:
    return lambda db: _agg_err_full(db, [{"$addFields": {"z": spec}}])


PROPAGATE_CASES: list[tuple[str, list[dict], Callable[[Database], object]]] = [
    ("cond-then", ONE_A, _z({"$cond": [True, "$nosuch", 1]})),
    ("cond-else", ONE_A, _z({"$cond": [False, 1, "$nosuch"]})),
    ("cond-taken-present", ONE_A, _z({"$cond": [True, 1, "$nosuch"]})),
    ("cond-nested", ONE_A, _z({"$cond": [True, {"$cond": [True, "$nosuch", 1]}, 2]})),
    ("cond-object-form", ONE_A, _z({"$cond": {"if": True, "then": "$nosuch", "else": 1}})),
    ("cond-remove", ONE_A, _z({"$cond": [True, "$$REMOVE", 1]})),
    ("switch-branch", ONE_A, _z({"$switch": {"branches": [{"case": True, "then": "$nosuch"}]}})),
    (
        "switch-default",
        ONE_A,
        _z({"$switch": {"branches": [{"case": False, "then": 1}], "default": "$nosuch"}}),
    ),
    (
        "switch-taken-present",
        ONE_A,
        _z({"$switch": {"branches": [{"case": True, "then": 5}], "default": "$nosuch"}}),
    ),
    ("let-in", ONE_A, _z({"$let": {"vars": {"v": 1}, "in": "$nosuch"}})),
    ("let-var-used", ONE_A, _z({"$let": {"vars": {"v": "$nosuch"}, "in": "$$v"}})),
    ("let-var-unused", ONE_A, _z({"$let": {"vars": {"v": "$nosuch"}, "in": 7}})),
    ("ifnull-all-missing", ONE_A, _z({"$ifNull": ["$n1", "$n2"]})),
    ("ifnull-second-present", ONE_A, _z({"$ifNull": ["$n1", 9]})),
    ("ifnull-last-null", ONE_A, _z({"$ifNull": ["$n1", None]})),
    ("ifnull-three-missing", ONE_A, _z({"$ifNull": ["$n1", "$n2", "$n3"]})),
    ("ifnull-null-then-missing", ONE_A, _z({"$ifNull": [None, "$nosuch"]})),
    ("nested-doc", ONE_A, _z({"k": {"$cond": [True, "$nosuch", 1]}})),
    ("in-array", ONE_A, _z([{"$cond": [True, "$nosuch", 1]}])),
    # The computing operators must keep collapsing missing to null.
    ("add-collapses", ONE_A, _z({"$add": ["$nosuch", 1]})),
    ("concat-collapses", ONE_A, _z({"$concat": ["x", "$nosuch"]})),
    # Operator position: the propagated value is MISSING, not null.
    ("eq-cond-null", ONE_A, _z({"$eq": [{"$cond": [True, "$nosuch", 1]}, None]})),
    ("eq-path-null", ONE_A, _z({"$eq": ["$nosuch", None]})),
    ("lte-cond-literal", ONE_A, _z({"$lte": [{"$cond": [True, "$nosuch", 1]}, "lit"]})),
    (
        "eq-switch-null",
        ONE_A,
        _z({"$eq": [{"$switch": {"branches": [{"case": True, "then": "$nosuch"}]}}, None]}),
    ),
]

# The expression language's comparison and truthiness rules, found by the first
# operator-by-operator sweep of it (`tools/probes/agg_expressions.py`). Four
# separate defects, all silent wrong ANSWERS rather than wrong errors:
#
#   * `$gt`/`$gte`/`$lt`/`$lte` compared with Python's own operators and
#     swallowed the TypeError a cross-type pair raises, so EVERY comparison
#     between different BSON types answered false. `$cmp` had it right.
#   * `$and`/`$or` iterated a non-array argument, walking a STRING CHARACTER BY
#     CHARACTER, and mongod's truthiness makes every string true -- empty too.
#   * A bool is not a number: `{$eq: [true, 1]}` is false on mongod, and Python's
#     `True == 1` made it true (and collapsed `$addToSet`'s `0` and `false`).
#   * Missing is falsy; it was reaching the catch-all and reading as true.
#
# `sc-` are the short-circuit cases: mongod DOES short-circuit at runtime, so a
# false `$and` operand hides a later error -- but an all-constant version folds
# at optimization time and raises, which is why these use a field reference.
CMP_SEED = [{"_id": 1, "n": 1, "s": "abc", "z0": 0}]


def _r(expr: object) -> Callable[[Database], object]:
    return lambda db: _agg_err_full(db, [{"$addFields": {"r": expr}}])


EXPRCMP_CASES: list[tuple[str, list[dict], Callable[[Database], object]]] = [
    ("gt-str-int", CMP_SEED, _r({"$gt": ["abc", 1]})),
    ("gte-str-int", CMP_SEED, _r({"$gte": ["abc", 1]})),
    ("lt-null-int", CMP_SEED, _r({"$lt": [None, 1]})),
    ("lte-null-int", CMP_SEED, _r({"$lte": [None, 1]})),
    ("gt-bool-int", CMP_SEED, _r({"$gt": [True, 1]})),
    ("lt-bool-date", CMP_SEED, _r({"$lt": [True, datetime(2026, 1, 2)]})),
    (
        "gt-date-oid",
        CMP_SEED,
        _r({"$gt": [datetime(2026, 1, 2), ObjectId("0123456789ab0123456789ab")]}),
    ),
    ("lt-obj-arr", CMP_SEED, _r({"$lt": [{"$literal": {"k": 1}}, {"$literal": [1]}]})),
    ("gt-emptystr-int", CMP_SEED, _r({"$gt": ["", 1]})),
    ("gt-missing-int", CMP_SEED, _r({"$gt": ["$nosuch", 1]})),
    ("lt-missing-int", CMP_SEED, _r({"$lt": ["$nosuch", 1]})),
    ("cmp-agrees-with-lt", CMP_SEED, _r({"$cmp": ["abc", 1]})),
    # bool is not a number
    ("eq-bool-int", CMP_SEED, _r({"$eq": [True, 1]})),
    ("ne-bool-int", CMP_SEED, _r({"$ne": [True, 1]})),
    ("eq-int-double", CMP_SEED, _r({"$eq": [1, 1.0]})),
    ("in-bool-numbers", CMP_SEED, _r({"$in": [False, {"$literal": [0, 44]}]})),
    (
        "setunion-bool-zero",
        CMP_SEED,
        _r({"$setUnion": [{"$literal": [0, False]}, {"$literal": [44]}]}),
    ),
    # truthiness
    ("or-empty-string", CMP_SEED, _r({"$or": ""})),
    ("and-string-field", CMP_SEED, _r({"$and": "$s"})),
    ("or-missing", CMP_SEED, _r({"$or": "$nosuch"})),
    ("and-missing", CMP_SEED, _r({"$and": "$nosuch"})),
    ("and-empty-array", CMP_SEED, _r({"$and": []})),
    ("or-empty-array", CMP_SEED, _r({"$or": []})),
    ("and-zero", CMP_SEED, _r({"$and": 0})),
    ("cond-empty-string", CMP_SEED, _r({"$cond": ["", "T", "F"]})),
    ("not-empty-string", CMP_SEED, _r({"$not": ""})),
    ("tobool-empty-string", CMP_SEED, _r({"$toBool": ""})),
    # short-circuiting, which the fix must not lose
    ("sc-and-false-hides-error", CMP_SEED, _r({"$and": [False, {"$divide": ["$n", "$z0"]}]})),
    (
        "sc-or-true-hides-error",
        CMP_SEED,
        _r({"$or": [{"$lt": ["$n", 5]}, {"$divide": ["$n", "$z0"]}]}),
    ),
    (
        "sc-and-true-raises",
        CMP_SEED,
        _r({"$and": [{"$lt": ["$n", 5]}, {"$divide": ["$n", "$z0"]}]}),
    ),
]

# The undefined-`$$variable` walker reached the aggregation pipeline; every
# OTHER surface that takes a filter or an update still answered the storage
# layer's generic BadValue (2) on the Rust server. `find` / `count` / `distinct`
# / `findAndModify` error at the command level, while `update` / `delete` report
# it PER STATEMENT in `writeErrors` -- so an earlier statement in the batch still
# applies. A pipeline-form update carries the `Invalid $<stage>` wrapper; a
# filter never does. Probed on mongod 8.2.11.
UV_SEED = [{"_id": 1, "a": 1, "s": "$$NOPE"}]
UV_EXPR = {"$expr": {"$eq": ["$$NOPE", 1]}}


def _c(cmd: dict) -> Callable[[Database], object]:
    """Run a raw command, keeping any per-statement writeError."""

    def run(db: Database) -> object:
        from pymongo.errors import OperationFailure

        try:
            r = db.command(dict(cmd))
        except OperationFailure as exc:
            return f"{exc.code}: {str(exc.details.get('errmsg', ''))}"
        we = (r.get("writeErrors") or [{}])[0]
        if we:
            return f"we{we.get('index')}/{we.get('code')}: {we.get('errmsg')} n={r.get('n')}"
        return f"ok n={r.get('n')}"

    return run


UNDEFVAR_CMD_CASES: list[tuple[str, list[dict], Callable[[Database], object]]] = [
    ("find", UV_SEED, _c({"find": "c", "filter": UV_EXPR})),
    ("find-nested-and", UV_SEED, _c({"find": "c", "filter": {"$and": [UV_EXPR]}})),
    ("count", UV_SEED, _c({"count": "c", "query": UV_EXPR})),
    ("distinct", UV_SEED, _c({"distinct": "c", "key": "a", "query": UV_EXPR})),
    (
        "fam-query",
        UV_SEED,
        _c({"findAndModify": "c", "query": UV_EXPR, "update": {"$set": {"b": 1}}}),
    ),
    (
        "fam-pipeline",
        UV_SEED,
        _c({"findAndModify": "c", "query": {}, "update": [{"$set": {"b": "$$NOPE"}}]}),
    ),
    (
        "update-q",
        UV_SEED,
        _c({"update": "c", "updates": [{"q": UV_EXPR, "u": {"$set": {"b": 1}}}]}),
    ),
    (
        "update-pipeline",
        UV_SEED,
        _c({"update": "c", "updates": [{"q": {}, "u": [{"$set": {"b": "$$NOPE"}}]}]}),
    ),
    # The error is per STATEMENT: the first update still applies (`n: 1`).
    (
        "update-second-statement",
        UV_SEED,
        _c(
            {
                "update": "c",
                "updates": [
                    {"q": {}, "u": {"$set": {"ok": 1}}},
                    {"q": UV_EXPR, "u": {"$set": {"b": 1}}},
                ],
            }
        ),
    ),
    ("delete-q", UV_SEED, _c({"delete": "c", "deletes": [{"q": UV_EXPR, "limit": 0}]})),
    # False-positive guards: a filter is query language, and `let` binds.
    ("ok-literal-in-filter", UV_SEED, _c({"find": "c", "filter": {"s": "$$NOPE"}})),
    (
        "ok-command-let",
        UV_SEED,
        _c({"find": "c", "filter": {"$expr": {"$eq": ["$$cv", 1]}}, "let": {"cv": 1}}),
    ),
    ("ok-plain-find", UV_SEED, _c({"find": "c", "filter": {"a": 1}})),
    (
        "ok-plain-update",
        UV_SEED,
        _c({"update": "c", "updates": [{"q": {"a": 1}, "u": {"$set": {"b": 2}}}]}),
    ),
]

# Fixed-arity operators: mongod answers 16020 `Expression $x takes exactly N
# arguments. M were passed in.` for 65 of them. We answered a mix of 14 / 28765 /
# 51044 / 51276 and, for 233 of the ~907 shapes, CRASHED with `internal server
# error` — an operator indexing `arg[0], arg[1]` on a scalar. It is a PARSE
# error: an empty or missing collection still reports it.
#
# The arity table was DERIVED by asking mongod each operator with 0-4 arguments
# and reading the count out of its own message, not taken from documentation.
# The count is `len(arg)` for an array and 1 for anything else; `$cond`'s object
# form is exempt; `$substr` is reported under its canonical name `$substrBytes`.
ARITY_SEED = [{"_id": 1, "a": 1}]


def _e(expr: object) -> Callable[[Database], object]:
    return lambda db: _agg_err_full(db, [{"$addFields": {"z": expr}}])


ARITY_CASES: list[tuple[str, list[dict], Callable[[Database], object]]] = [
    # arity 1 — these CRASHED
    ("abs-two-args", ARITY_SEED, _e({"$abs": [1, 2]})),
    ("abs-zero-args", ARITY_SEED, _e({"$abs": []})),
    ("abs-bare-ok", ARITY_SEED, _e({"$abs": 5})),
    ("abs-one-array-ok", ARITY_SEED, _e({"$abs": [5]})),
    ("abs-nested-expr-ok", ARITY_SEED, _e({"$abs": {"$add": [1, 2]}})),
    ("not-two-args", ARITY_SEED, _e({"$not": [True, True]})),
    ("type-doc-arg-ok", ARITY_SEED, _e({"$type": {"$literal": {"k": 1}}})),
    ("size-bare-ok", ARITY_SEED, _e({"$size": {"$literal": [1, 2]}})),
    # arity 2
    ("eq-one-arg", ARITY_SEED, _e({"$eq": [1]})),
    ("eq-three-args", ARITY_SEED, _e({"$eq": [1, 2, 3]})),
    ("eq-bare", ARITY_SEED, _e({"$eq": 1})),
    ("divide-one-arg", ARITY_SEED, _e({"$divide": [1]})),
    ("split-one-arg", ARITY_SEED, _e({"$split": ["a"]})),
    ("arrayelemat-one-arg", ARITY_SEED, _e({"$arrayElemAt": [1]})),
    # arity 3, and $cond's object form which must stay exempt
    ("cond-two-args", ARITY_SEED, _e({"$cond": [True, 1]})),
    ("cond-three-args-ok", ARITY_SEED, _e({"$cond": [True, 1, 2]})),
    ("cond-object-form-ok", ARITY_SEED, _e({"$cond": {"if": True, "then": 1, "else": 2}})),
    # `$substr` reports as `$substrBytes`
    ("substr-alias-name", ARITY_SEED, _e({"$substr": 1})),
    ("substrbytes-two-args", ARITY_SEED, _e({"$substrBytes": ["ab", 0]})),
    # PARSE time: an empty (here, missing) collection still reports it.
    ("empty-collection", [], _e({"$abs": [1, 2]})),
    # The wrapper follows the stage, as for undefined variables.
    (
        "bare-in-group",
        ARITY_SEED,
        lambda db: _agg_err_full(db, [{"$group": {"_id": {"$abs": [1, 2]}}}]),
    ),
]

# Decimal128 through the math operators. Thirteen of them CRASHED the Python
# server (`internal server error`) because `math` rejects a `Decimal128`
# outright, and the rest narrowed it to a `float`, dropping half the digits
# mongod keeps. decimal128 carries 34 significant digits and mongod computes in
# them; `float` carries 17.
#
# The hyperbolics are exact identities over exp/ln/sqrt, which `decimal`
# provides. The CIRCULAR functions have no such identity and still narrow —
# recorded in `tasks/backlog.md`, not fixed here.
DEC = Decimal128("2.5")
DEC_SEED = [{"_id": 1}]


def _d(expr: object) -> Callable[[Database], object]:
    return lambda db: _agg_err_full(db, [{"$addFields": {"z": expr}}])


DECIMAL_CASES: list[tuple[str, list[dict], Callable[[Database], object]]] = [
    # these thirteen crashed
    ("abs", DEC_SEED, _d({"$abs": DEC})),
    ("ceil", DEC_SEED, _d({"$ceil": DEC})),
    ("floor", DEC_SEED, _d({"$floor": DEC})),
    ("trunc", DEC_SEED, _d({"$trunc": DEC})),
    ("round", DEC_SEED, _d({"$round": DEC})),
    ("exp", DEC_SEED, _d({"$exp": DEC})),
    ("ln", DEC_SEED, _d({"$ln": DEC})),
    ("log10", DEC_SEED, _d({"$log10": DEC})),
    ("sqrt", DEC_SEED, _d({"$sqrt": DEC})),
    ("mod", DEC_SEED, _d({"$mod": [DEC, 2]})),
    ("pow", DEC_SEED, _d({"$pow": [DEC, 2]})),
    ("log", DEC_SEED, _d({"$log": [DEC, 2]})),
    ("avg", DEC_SEED, _d({"$avg": [DEC, DEC]})),
    # precision, not crashes
    ("sinh", DEC_SEED, _d({"$sinh": DEC})),
    ("cosh", DEC_SEED, _d({"$cosh": DEC})),
    ("tanh", DEC_SEED, _d({"$tanh": DEC})),
    ("asinh", DEC_SEED, _d({"$asinh": DEC})),
    ("degrees-to-radians", DEC_SEED, _d({"$degreesToRadians": DEC})),
    ("radians-to-degrees", DEC_SEED, _d({"$radiansToDegrees": DEC})),
    # the conversions and predicates, which were already right
    ("to-decimal", DEC_SEED, _d({"$toDecimal": DEC})),
    ("to-string", DEC_SEED, _d({"$toString": DEC})),
    ("type", DEC_SEED, _d({"$type": DEC})),
    ("is-number", DEC_SEED, _d({"$isNumber": DEC})),
    ("add", DEC_SEED, _d({"$add": [DEC, 2]})),
    ("multiply", DEC_SEED, _d({"$multiply": [DEC, 2]})),
    ("divide", DEC_SEED, _d({"$divide": [DEC, 2]})),
    ("eq-decimal-int", DEC_SEED, _d({"$eq": [Decimal128("2"), 2]})),
]

# Operators whose argument must be a DOCUMENT. 25 of them, 675 shapes, each with
# mongod's own code and one of FIVE phrasings that are not interchangeable
# ("found: <T>" / "found <T>" / no type at all). The Rust server answered a
# generic BadValue (2) for every one; a table is the only thing that reproduces
# them. Parse-time, like the arity check.
#
# The last of the sweep's CRASHES are here too: an unrecognised key in `$cond` /
# `$dateToString` was a bare KeyError, `{$trunc: []}` an IndexError, and
# `$exp`/`$sinh`/`$cosh` of a large value an OverflowError — mongod saturates to
# infinity. That takes the Python server from 274 crashes to zero.
OBJ_SEED = [{"_id": 1}]


def _o(expr: object) -> Callable[[Database], object]:
    return lambda db: _agg_err_full(db, [{"$addFields": {"z": expr}}])


OBJECT_ARG_CASES: list[tuple[str, list[dict], Callable[[Database], object]]] = [
    # the five phrasings, one operator each
    ("let-int", OBJ_SEED, _o({"$let": 5})),
    ("reduce-int", OBJ_SEED, _o({"$reduce": 5})),
    ("trim-int", OBJ_SEED, _o({"$trim": 5})),
    ("convert-int", OBJ_SEED, _o({"$convert": 5})),
    ("dateadd-int", OBJ_SEED, _o({"$dateAdd": 5})),
    # the type name is rendered per value kind
    ("reduce-string", OBJ_SEED, _o({"$reduce": "x"})),
    ("reduce-bool", OBJ_SEED, _o({"$reduce": True})),
    ("reduce-null", OBJ_SEED, _o({"$reduce": None})),
    ("reduce-array", OBJ_SEED, _o({"$reduce": [1]})),
    # a spread of the remaining operators
    ("filter-int", OBJ_SEED, _o({"$filter": 5})),
    ("map-int", OBJ_SEED, _o({"$map": 5})),
    ("switch-int", OBJ_SEED, _o({"$switch": 5})),
    ("zip-int", OBJ_SEED, _o({"$zip": 5})),
    ("regexmatch-int", OBJ_SEED, _o({"$regexMatch": 5})),
    ("replaceall-int", OBJ_SEED, _o({"$replaceAll": 5})),
    ("sortarray-int", OBJ_SEED, _o({"$sortArray": 5})),
    ("setfield-int", OBJ_SEED, _o({"$setField": 5})),
    ("datetostring-int", OBJ_SEED, _o({"$dateToString": 5})),
    ("datetrunc-int", OBJ_SEED, _o({"$dateTrunc": 5})),
    ("datefromstring-int", OBJ_SEED, _o({"$dateFromString": 5})),
    # parse-time: a missing collection still reports it
    ("let-int-empty-collection", [], _o({"$let": 5})),
    # ranged arity
    ("trunc-zero-args", OBJ_SEED, _o({"$trunc": []})),
    ("trunc-three-args", OBJ_SEED, _o({"$trunc": [1, 2, 3]})),
    ("round-zero-args", OBJ_SEED, _o({"$round": []})),
    ("trunc-two-args-ok", OBJ_SEED, _o({"$trunc": [1.55, 1]})),
    # unrecognised keys — were bare KeyErrors
    ("cond-unknown-key", OBJ_SEED, _o({"$cond": {"k": 1}})),
    ("datetostring-unknown-key", OBJ_SEED, _o({"$dateToString": {"k": 1}})),
    ("cond-object-ok", OBJ_SEED, _o({"$cond": {"if": True, "then": 1, "else": 2}})),
    # overflow saturates to infinity
    ("exp-overflow", OBJ_SEED, _o({"$exp": 1099511627776})),
    ("sinh-overflow", OBJ_SEED, _o({"$sinh": 1099511627776})),
    ("cosh-overflow", OBJ_SEED, _o({"$cosh": 1099511627776})),
    ("sinh-ok", OBJ_SEED, _o({"$sinh": 1})),
]

# mongod's numeric type guard, `$OP only supports numeric types, not <type>` --
# 24 unary operators, 220 shapes, the largest family left in the sweep. The Rust
# engine deferred every one to a generic BadValue, because the operators know
# the operand's type when they evaluate it but `Fallback` carries no code.
#
# The Python server already answered these correctly, so these cases pin BOTH
# servers against a family only one of them had.
NG_SEED = [{"_id": 1, "s": "x", "n": 2}]


def _n(expr: object) -> Callable[[Database], object]:
    return lambda db: _agg_err_full(db, [{"$addFields": {"z": expr}}])


NUMERIC_GUARD_CASES: list[tuple[str, list[dict], Callable[[Database], object]]] = [
    # a FIELD reference — evaluated per document, so this is the runtime path
    ("abs-field-string", NG_SEED, _n({"$abs": "$s"})),
    ("sqrt-field-string", NG_SEED, _n({"$sqrt": "$s"})),
    ("ln-field-string", NG_SEED, _n({"$ln": "$s"})),
    ("ceil-field-string", NG_SEED, _n({"$ceil": "$s"})),
    ("floor-field-string", NG_SEED, _n({"$floor": "$s"})),
    ("sin-field-string", NG_SEED, _n({"$sin": "$s"})),
    ("exp-field-string", NG_SEED, _n({"$exp": "$s"})),
    # `$round` / `$trunc` answer 51081 where the rest answer 28765
    ("round-field-string", NG_SEED, _n({"$round": "$s"})),
    ("trunc-field-string", NG_SEED, _n({"$trunc": "$s"})),
    # nested inside another expression
    ("nested-in-add", NG_SEED, _n({"$add": [1, {"$abs": "$s"}]})),
    # a null operand is NOT an error, and a numeric one computes
    ("abs-null-ok", NG_SEED, _n({"$abs": None})),
    ("abs-number-ok", NG_SEED, _n({"$abs": -3})),
    ("abs-field-number-ok", NG_SEED, _n({"$abs": "$n"})),
    ("sqrt-field-number-ok", NG_SEED, _n({"$sqrt": "$n"})),
]

# Which of mongod's two prefixes an expression error carries. A CONSTANT
# expression is folded at optimization time -- `Failed to optimize pipeline ::
# caused by ::` -- and a document-dependent one fails per document under
# `Executor error during aggregate command on namespace: … :: caused by ::`.
# Both servers always used the executor form: 618 of the Python server's
# message-only differences and 148 of the Rust server's.
#
# The predicate is "does it read the document": a field path, `$$ROOT` /
# `$$CURRENT`, a variable bound from the input and `$rand` are execution-time;
# literals, `$$NOW` and the command's own `let` values fold. Each pair below is
# the same error reached both ways, so the two prefixes are pinned against each
# other rather than in isolation.
FOLD_SEED = [{"_id": 1, "s": "x", "n": 2}]


def _f(expr: object) -> Callable[[Database], object]:
    return lambda db: _agg_err_full(db, [{"$addFields": {"z": expr}}])


FOLD_CASES: list[tuple[str, list[dict], Callable[[Database], object]]] = [
    # constant -> folded
    ("const-abs", FOLD_SEED, _f({"$abs": "x"})),
    ("const-divide-by-zero", FOLD_SEED, _f({"$divide": [1, 0]})),
    ("const-nested", FOLD_SEED, _f({"$abs": {"$concat": ["a", "b"]}})),
    ("const-literal", FOLD_SEED, _f({"$abs": {"$literal": "x"}})),
    ("const-now", FOLD_SEED, _f({"$abs": "$$NOW"})),
    ("const-sqrt", FOLD_SEED, _f({"$sqrt": "x"})),
    # document-dependent -> executor
    ("field-abs", FOLD_SEED, _f({"$abs": "$s"})),
    ("field-divide-by-zero", FOLD_SEED, _f({"$divide": ["$n", 0]})),
    ("field-nested", FOLD_SEED, _f({"$abs": {"$concat": ["a", "$s"]}})),
    ("root-var", FOLD_SEED, _f({"$abs": "$$ROOT"})),
    ("current-var", FOLD_SEED, _f({"$abs": "$$CURRENT"})),
    ("rand-is-not-constant", FOLD_SEED, _f({"$abs": {"$toString": {"$rand": {}}}})),
    (
        "map-binding-is-not-constant",
        FOLD_SEED,
        _f({"$map": {"input": [["x"]], "in": {"$abs": "$$this"}}}),
    ),
    # `$let` folds only when its bindings do
    ("let-constant-binding", FOLD_SEED, _f({"$let": {"vars": {"v": "x"}, "in": {"$abs": "$$v"}}})),
    ("let-field-binding", FOLD_SEED, _f({"$let": {"vars": {"v": "$s"}, "in": {"$abs": "$$v"}}})),
    # a successful constant is still just computed
    ("const-ok", FOLD_SEED, _f({"$abs": -3})),
    ("field-ok", FOLD_SEED, _f({"$abs": "$n"})),
]

# The BSON *type* an expression answers, alongside its value. These bugs were
# invisible to a value-only comparison -- `$trunc` of a long answered the right
# number as the wrong type -- so every case here pins `$type` too.
#
# Three rules, all probed on 8.2.11:
#   * the rounding operators are type-preserving (a double in is a double out,
#     `$ceil` of 1.5 is 2.0);
#   * `long` is contagious through arithmetic, and an int32 result that
#     outgrows its width widens to long;
#   * an integral result past int64 saturates to a **double** in an
#     aggregation (`$pow: [2, 64]`), where the *update* operators fail --
#     see `UPDATE_OVERFLOW_CASES`.
NUMTYPE_SEED = [{"_id": 1, "n": 2, "big": Int64(9223372036854775807), "small": Int64(5)}]


def _tv(expr: object) -> Callable[[Database], object]:
    from pymongo.errors import OperationFailure

    def run(db: Database) -> object:
        try:
            doc = list(db.c.aggregate([{"$addFields": {"z": expr, "zt": {"$type": expr}}}]))[0]
            return f"{doc['zt']}={doc['z']!r}"
        except OperationFailure as exc:
            msg = re.sub(r"namespace: [^ ]+", "namespace: <ns>", str(exc.details.get("errmsg", "")))
            return f"{exc.code}/{_stable_code_name(exc.details)}: {msg}"

    return run


NUMTYPE_CASES: list[tuple[str, list[dict], Callable[[Database], object]]] = [
    # type-preserving rounding
    ("ceil-double", NUMTYPE_SEED, _tv({"$ceil": 1.5})),
    ("floor-double", NUMTYPE_SEED, _tv({"$floor": 1.5})),
    ("trunc-double", NUMTYPE_SEED, _tv({"$trunc": 1.5})),
    ("round-double", NUMTYPE_SEED, _tv({"$round": 1.5})),
    ("trunc-int", NUMTYPE_SEED, _tv({"$trunc": 1})),
    ("trunc-long", NUMTYPE_SEED, _tv({"$trunc": "$small"})),
    ("floor-long", NUMTYPE_SEED, _tv({"$floor": "$small"})),
    ("ceil-long", NUMTYPE_SEED, _tv({"$ceil": "$small"})),
    ("round-long", NUMTYPE_SEED, _tv({"$round": "$small"})),
    ("abs-long", NUMTYPE_SEED, _tv({"$abs": "$small"})),
    # long is contagious
    ("add-long-int", NUMTYPE_SEED, _tv({"$add": ["$small", 1]})),
    ("subtract-long-int", NUMTYPE_SEED, _tv({"$subtract": ["$small", 1]})),
    ("multiply-long-int", NUMTYPE_SEED, _tv({"$multiply": ["$small", 2]})),
    ("mod-long-int", NUMTYPE_SEED, _tv({"$mod": ["$small", 2]})),
    ("pow-long-int", NUMTYPE_SEED, _tv({"$pow": ["$small", 2]})),
    ("add-int-int", NUMTYPE_SEED, _tv({"$add": [1, 2]})),
    # int32 overflow widens to long
    ("add-int32-overflow", NUMTYPE_SEED, _tv({"$add": [2147483647, 1]})),
    ("multiply-int32-overflow", NUMTYPE_SEED, _tv({"$multiply": [2147483647, 2]})),
    ("abs-int32-min", NUMTYPE_SEED, _tv({"$abs": -2147483648})),
    # int64 overflow saturates to a double
    ("add-int64-overflow", NUMTYPE_SEED, _tv({"$add": ["$big", 1]})),
    ("multiply-int64-overflow", NUMTYPE_SEED, _tv({"$multiply": ["$big", 2]})),
    ("pow-int64-overflow", NUMTYPE_SEED, _tv({"$pow": [2, 64]})),
    ("pow-huge-is-inf", NUMTYPE_SEED, _tv({"$pow": [10, 400]})),
    # `$mod` truncates toward zero, so the sign follows the DIVIDEND
    ("mod-neg-dividend", NUMTYPE_SEED, _tv({"$mod": [-5, 2]})),
    ("mod-neg-divisor", NUMTYPE_SEED, _tv({"$mod": [5, -2]})),
    ("mod-both-neg", NUMTYPE_SEED, _tv({"$mod": [-5, -2]})),
    ("mod-double", NUMTYPE_SEED, _tv({"$mod": [-5.5, 2]})),
    # the degree/radian factor is precomputed, which shows in the last bit
    ("degrees-to-radians", NUMTYPE_SEED, _tv({"$degreesToRadians": 1.5})),
    ("radians-to-degrees", NUMTYPE_SEED, _tv({"$radiansToDegrees": 1.5})),
    ("degrees-to-radians-int", NUMTYPE_SEED, _tv({"$degreesToRadians": 7})),
]

# `$toLower` / `$toUpper` coerce their operand to a string first, and that
# conversion is NOT `$toString`'s: it takes a javascript value but rejects a
# bool and an ObjectId, renders a double through `%g` (six significant digits)
# where `$toString` round-trips it, and turns null and missing into `""` where
# `$toString` gives null. Both probed against 8.2.11.
STR_SEED = [{"_id": 1, "n": 2, "s": "aB", "d": 1099511627776.0, "arr": [1], "o": {"k": 1}}]

STRCONV_CASES: list[tuple[str, list[dict], Callable[[Database], object]]] = [
    ("lower-int", STR_SEED, _tv({"$toLower": 1})),
    ("lower-negative", STR_SEED, _tv({"$toLower": -1})),
    ("lower-double-fraction", STR_SEED, _tv({"$toLower": 1.5})),
    ("lower-double-whole", STR_SEED, _tv({"$toLower": 4.0})),
    ("lower-double-wide", STR_SEED, _tv({"$toLower": "$d"})),
    ("lower-decimal", STR_SEED, _tv({"$toLower": Decimal128("2.5")})),
    ("lower-null", STR_SEED, _tv({"$toLower": None})),
    ("lower-missing", STR_SEED, _tv({"$toLower": "$nosuch"})),
    ("lower-date", STR_SEED, _tv({"$toLower": datetime(2026, 1, 2, 3, 4, 5)})),
    ("lower-field", STR_SEED, _tv({"$toLower": "$s"})),
    ("lower-bool-rejected", STR_SEED, _tv({"$toLower": True})),
    ("lower-oid-rejected", STR_SEED, _tv({"$toLower": ObjectId("64b7f9a2c1d2e3f4a5b6c7d8")})),
    ("lower-array-rejected", STR_SEED, _tv({"$toLower": "$arr"})),
    ("lower-object-rejected", STR_SEED, _tv({"$toLower": "$o"})),
    ("upper-double-wide", STR_SEED, _tv({"$toUpper": "$d"})),
    ("upper-field", STR_SEED, _tv({"$toUpper": "$s"})),
    ("upper-date", STR_SEED, _tv({"$toUpper": datetime(2026, 1, 2, 3, 4, 5)})),
    ("upper-missing", STR_SEED, _tv({"$toUpper": "$nosuch"})),
    # `$toString` is the other conversion
    ("tostring-true", STR_SEED, _tv({"$toString": True})),
    ("tostring-false", STR_SEED, _tv({"$toString": False})),
    ("tostring-date", STR_SEED, _tv({"$toString": datetime(2026, 1, 2, 3, 4, 5)})),
    ("tostring-double-wide", STR_SEED, _tv({"$toString": "$d"})),
    ("tostring-double-whole", STR_SEED, _tv({"$toString": 4.0})),
    ("tostring-oid", STR_SEED, _tv({"$toString": ObjectId("64b7f9a2c1d2e3f4a5b6c7d8")})),
    ("tostring-null", STR_SEED, _tv({"$toString": None})),
    ("tostring-missing", STR_SEED, _tv({"$toString": "$nosuch"})),
    ("tostring-array-rejected", STR_SEED, _tv({"$toString": "$arr"})),
    ("tostring-object-rejected", STR_SEED, _tv({"$toString": "$o"})),
    # the same `%g` rendering inside an error message
    ("acos-message-renders-wide-double", STR_SEED, _tv({"$acos": "$d"})),
]

# `$inc` / `$mul` past int64 FAIL the write -- they do not saturate to a double
# the way the aggregation operators do. Probed 8.2.11. Both servers used to let
# the unbounded Python int reach `bson.encode`, whose `OverflowError` escaped
# from inside the storage layer's update transaction as an internal error.
OVF_SEED = [
    {"_id": 1, "n": Int64(9223372036854775807)},
    {"_id": 2, "n": Int64(-9223372036854775808)},
    {"_id": 3, "n": Int64(5)},
]


def _u(query: dict, update: dict) -> Callable[[Database], object]:
    from pymongo.errors import OperationFailure

    def run(db: Database) -> object:
        try:
            db.c.update_one(query, update)
            doc = db.c.find_one(query)
            return f"ok:{doc['n']!r}" if doc else "ok:none"
        except OperationFailure as exc:
            return f"{exc.code}/{_stable_code_name(exc.details)}: {exc.details.get('errmsg', '')}"

    return run


UPDATE_OVERFLOW_CASES: list[tuple[str, list[dict], Callable[[Database], object]]] = [
    ("inc-past-max", OVF_SEED, _u({"_id": 1}, {"$inc": {"n": 1}})),
    ("inc-past-max-by-long", OVF_SEED, _u({"_id": 1}, {"$inc": {"n": Int64(5)}})),
    ("mul-past-max", OVF_SEED, _u({"_id": 1}, {"$mul": {"n": 2}})),
    ("inc-past-min", OVF_SEED, _u({"_id": 2}, {"$inc": {"n": -1}})),
    # a double operand moves the whole thing into the double domain -- no overflow
    ("inc-by-double-widens", OVF_SEED, _u({"_id": 1}, {"$inc": {"n": 1.0}})),
    # and the ordinary case still works, keeping its width
    ("inc-in-range", OVF_SEED, _u({"_id": 3}, {"$inc": {"n": 1}})),
    ("mul-in-range", OVF_SEED, _u({"_id": 3}, {"$mul": {"n": 2}})),
]


ALL_CASES = (
    [("query", c) for c in QUERY_CASES]
    + [("readpath", c) for c in READPATH_CASES]
    + [("update", c) for c in UPDATE_CASES]
    + [("fam", c) for c in FAM_CASES]
    + [("updatecmd", c) for c in UPDATE_CMD_CASES]
    + [("cursor", c) for c in CURSOR_CASES]
    + [("hint", c) for c in HINT_CASES]
    + [("lookup", c) for c in LOOKUP_CASES]
    + [("maxtime", c) for c in MAXTIME_CASES]
    + [("aggerr", c) for c in AGGERR_CASES]
    + [("redact", c) for c in REDACT_CASES]
    + [("undefvar", c) for c in UNDEFVAR_CASES]
    + [("remove", c) for c in REMOVE_CASES]
    + [("propagate", c) for c in PROPAGATE_CASES]
    + [("exprcmp", c) for c in EXPRCMP_CASES]
    + [("undefvarcmd", c) for c in UNDEFVAR_CMD_CASES]
    + [("arity", c) for c in ARITY_CASES]
    + [("decimal", c) for c in DECIMAL_CASES]
    + [("objarg", c) for c in OBJECT_ARG_CASES]
    + [("numguard", c) for c in NUMERIC_GUARD_CASES]
    + [("fold", c) for c in FOLD_CASES]
    + [("numtype", c) for c in NUMTYPE_CASES]
    + [("strconv", c) for c in STRCONV_CASES]
    + [("updovf", c) for c in UPDATE_OVERFLOW_CASES]
)


@requires_mongod
@pytest.mark.parametrize("kind,case", ALL_CASES, ids=[f"{k}-{c[0]}" for k, c in ALL_CASES])
def test_matches_mongod(
    kind, case, secantus_uri: str, mongod_uri: str, mongod_version: tuple[int, int]
) -> None:
    """SecantusDB must answer exactly what mongod answers."""
    name, seed, op = case
    if mongod_version[0] != PROBED_MONGOD_MAJOR:
        found = ".".join(str(p) for p in mongod_version)
        pytest.skip(
            f"this gate asserts an exact match against mongod "
            f"{PROBED_MONGOD_MAJOR}.x (probed {PROBED_MONGOD_VERSION}), and this "
            f"box has mongod {found}; across majors its error surface differs in "
            f"ways that are not SecantusDB divergences. See PROBED_MONGOD_MAJOR."
        )
    db_name = f"diff_{kind}_{name.replace('-', '_')}"
    ours = _run(secantus_uri, db_name, seed, op)
    theirs = _run(mongod_uri, db_name, seed, op)
    assert ours == theirs, f"{name}: mongod={theirs} ours={ours}"
