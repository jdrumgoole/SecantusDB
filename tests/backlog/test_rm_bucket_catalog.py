"""Gap tests for the time-series bucket catalog's `serverStatus` section,
against mongod 8.2.11 (RM-549, split 2026-11-16 into three items).

The catalog's counters are cumulative for the life of the process, so each
test starts a server of its own on each side (the module-scoped fixtures
share counters with every other test) and a probe runs on both.
"""

from __future__ import annotations

import contextlib
import datetime as dt
import pathlib
from collections.abc import Iterator
from typing import Any

import pymongo
import pytest

from tests.backlog.conftest import only_on, rust_mongo_server

IDLE_THRESHOLD = "timeseriesIdleBucketExpiryMemoryUsageThreshold"


@contextlib.contextmanager
def _fresh_pair(store: pathlib.Path) -> Iterator[tuple[Any, Any]]:
    """A new `secantusd-mdb` and a new mongod: `(rust, mongod)` clients."""
    from tests.mongod_reference import MONGOD, _start_mongod, _stop_mongod

    assert MONGOD, "no mongod on PATH: the reference server is required"
    with rust_mongo_server(store) as rust:
        proc, uri, dbpath = _start_mongod()
        ref = pymongo.MongoClient(uri, serverSelectionTimeoutMS=10_000)
        try:
            yield rust, ref
        finally:
            ref.close()
            _stop_mongod(proc, dbpath)


def _section(client: Any) -> dict:
    return client.admin.command("serverStatus").get("bucketCatalog", {})


def _make(client: Any) -> Any:
    db = client["rm_bucket_catalog"]
    db.create_collection("ts", timeseries={"timeField": "t", "metaField": "m"})
    return db.ts


def _without_memory(section: dict) -> list:
    """The section in mongod's field order, less the one value (`memoryUsage`)
    that is RM-710's: the key stays, so a section that lacks it
    still differs."""
    return [(k, "<bytes>" if k == "memoryUsage" else v) for k, v in section.items()]


@pytest.mark.backlog_fixed("RM-709")
def test_bucket_catalog_counts_and_counters_follow_the_writes(tmp_path: pathlib.Path) -> None:
    """mongod 8.2.11, a fresh server: no section before the first write;
    after four inserts over two metadata values (`numBuckets` 2,
    `numBucketInserts` 2, `numCommits` 2, `numMeasurementsCommitted` 4,
    `avgNumMeasurementsPerCommit` 2, `stateManagement` `{bucketsManaged: 2,
    currentEra: 0, erasWithRemainingBuckets: 1, trackedClearOperations: 0}`),
    and after one more measurement into an open bucket (the bucket updated:
    `numBucketUpdates` 1, `numBuckets` still 2). Every field but
    `memoryUsage` is compared, in order; this server reported no section
    before RM-709."""
    t = dt.datetime(2026, 1, 1)

    def probe(client: Any) -> list:
        steps = [_section(client)]
        coll = _make(client)
        steps.append(_section(client))
        coll.insert_many([{"t": t, "m": i % 2} for i in range(4)])
        steps.append(_without_memory(_section(client)))
        coll.insert_one({"t": t, "m": 0})
        steps.append(_without_memory(_section(client)))
        return steps

    with _fresh_pair(tmp_path) as (rust, ref):
        got, want = probe(rust), probe(ref)
    assert want[0] == {} and want[1] == {}
    assert dict(want[2])["numBuckets"] == 2
    assert dict(want[2])["stateManagement"]["bucketsManaged"] == 2
    assert dict(want[3])["numBucketUpdates"] == 1
    assert got == want


@pytest.mark.backlog_fixed("RM-710")
def test_bucket_catalog_memory_usage_is_the_bytes_mongod_accounts(tmp_path: pathlib.Path) -> None:
    """mongod charges the catalog's structures through tracking allocators,
    so `memoryUsage` is the bytes its C++ containers allocate, and those
    are the standard library's. Linux x86-64 mongod (the exemplar, built
    with libstdc++; 8.2.11 and 8.2.12, official amd64 image, measured
    2026-11-26) gives 13120 with one bucket holding one measurement and
    13396 after four more measurements (a new `v` field among them) join
    that bucket, both repeating run for run and in both versions. macOS
    arm64 mongod 8.2.11 (libc++) gives 12552 and 12828, the same +276
    step, so the comparison runs on Linux only, where CI checks it (Joe,
    2026-11-25: port mongod's charges so the counts match that build);
    `test_bucket_catalog_memory_usage_in_the_exemplar_bytes` pins the
    Linux numbers for this server on every host."""
    only_on(
        "linux",
        why="the counts are libstdc++'s container sizes; macOS mongod's libc++ gives 12552 / 12828",
    )
    t = dt.datetime(2026, 1, 1)

    def probe(client: Any) -> list:
        coll = _make(client)
        coll.insert_one({"t": t, "m": "a"})
        first = _section(client).get("memoryUsage")
        coll.insert_many(
            [{"t": t + dt.timedelta(seconds=i), "m": "a", "v": i} for i in range(1, 5)]
        )
        return [first, _section(client).get("memoryUsage")]

    with _fresh_pair(tmp_path) as (rust, ref):
        got, want = probe(rust), probe(ref)
    assert want == [13120, 13396]
    assert got == want


def _memory_after(client: Any, docs: list, *, meta: bool = True, driver_id: bool = False) -> Any:
    """`memoryUsage` after `docs` go into a new time-series collection of a
    fresh server. `driver_id` leaves the `_id` to the driver (`insert_one`
    adds one); otherwise the command is sent as it is, so there is none."""
    db = client["rm_bucket_catalog"]
    opts = {"timeField": "t", "metaField": "m"} if meta else {"timeField": "t"}
    db.create_collection("ts", timeseries=opts)
    if driver_id:
        for d in docs:
            db.ts.insert_one(d)
    else:
        db.command("insert", "ts", documents=docs)
    return _section(client).get("memoryUsage")


_T0 = dt.datetime(2026, 1, 1)
_T1 = _T0 + dt.timedelta(seconds=1)

# What Linux x86-64 mongod 8.2.11 and 8.2.12 give a fresh server after the
# measurements (the official `mongo` amd64 image, standalone, each case on
# a server of its own, each twice: `tools/probes/linux_bucket_catalog.py`
# and `linux_bucket_catalog_isolate.py`, 2026-11-26). The first group has
# the driver's `_id`, the second sends the documents as they are.
_LINUX_BYTES: list[tuple[str, list, dict, int]] = [
    ("driver: one measurement", [{"t": _T0, "m": "a"}], {"driver_id": True}, 13120),
    ("driver: meta 1", [{"t": _T0, "m": 1}], {"driver_id": True}, 13120),
    ("driver: meta 41 characters", [{"t": _T0, "m": "x" * 41}], {"driver_id": True}, 13120),
    ("driver: no metaField", [{"t": _T0}], {"driver_id": True, "meta": False}, 12592),
    ("driver: one extra field", [{"t": _T0, "m": "a", "x": 1}], {"driver_id": True}, 13132),
    (
        "driver: two extra fields",
        [{"t": _T0, "m": "a", "x": 1, "y": 2}],
        {"driver_id": True},
        16120,
    ),
    (
        "driver: a second measurement",
        [{"t": _T0, "m": "a"}, {"t": _T1, "m": "a"}],
        {"driver_id": True},
        13168,
    ),
    ("t and meta", [{"t": _T0, "m": "a"}], {}, 11604),
    ("t only", [{"t": _T0}], {"meta": False}, 11076),
    ("an int32 field", [{"t": _T0, "m": "a", "x": 1}], {}, 13104),
    ("an int64 field", [{"t": _T0, "m": "a", "x": 1 << 40}], {}, 13112),
    ("a double field", [{"t": _T0, "m": "a", "x": 1.5}], {}, 13112),
    ("a bool field", [{"t": _T0, "m": "a", "x": True}], {}, 13098),
    ("a null field", [{"t": _T0, "m": "a", "x": None}], {}, 13096),
    ("a 15 character string", [{"t": _T0, "m": "a", "x": "y" * 15}], {}, 13159),
    ("a 16 character string", [{"t": _T0, "m": "a", "x": "y" * 16}], {}, 13162),
    ("a 20 character string", [{"t": _T0, "m": "a", "x": "x" * 20}], {}, 13174),
    ("a 20 character field name", [{"t": _T0, "m": "a", "f" * 20: 1}], {}, 13208),
    ("an empty document field", [{"t": _T0, "m": "a", "x": {}}], {}, 13092),
    ("a document field", [{"t": _T0, "m": "a", "x": {"a": 1}}], {}, 13148),
    ("a two field document", [{"t": _T0, "m": "a", "x": {"a": 1, "b": 2}}], {}, 14479),
    ("an array field", [{"t": _T0, "m": "a", "x": [1, 2, 3]}], {}, 14498),
    ("meta of 400 characters", [{"t": _T0, "m": "s" * 400}], {}, 12100),
    ("meta of 497 characters", [{"t": _T0, "m": "s" * 497}], {}, 13124),
    ("meta of 2000 characters", [{"t": _T0, "m": "s" * 2000}], {}, 15172),
    (
        "meta document of 600 characters",
        [{"t": _T0, "m": {"a": 1, "b": "x" * 600}}],
        {},
        13124,
    ),
    (
        "five measurements in one call",
        [{"t": _T0 + dt.timedelta(seconds=i), "m": "a"} for i in range(5)],
        {},
        11700,
    ),
]


@pytest.mark.backlog_fixed("RM-710")
@pytest.mark.parametrize(
    ("docs", "options", "want"),
    [pytest.param(d, o, w, id=name) for name, d, o, w in _LINUX_BYTES],
)
def test_bucket_catalog_memory_usage_in_the_exemplar_bytes(
    tmp_path: pathlib.Path, docs: list, options: dict, want: int
) -> None:
    """`memoryUsage` for one bucket, case by case, is what the exemplar
    mongod (Linux x86-64: libstdc++ and Abseil's layouts) gives, whatever
    host this server runs on (RM-710): each charge is a structure mongod
    allocates, so a new field, a longer name or string, a document or array
    value, a long meta value each move it as they move mongod's."""
    with rust_mongo_server(tmp_path) as rust:
        assert _memory_after(rust, docs, **options) == want


def _sec(i: int) -> dt.datetime:
    return _T0 + dt.timedelta(seconds=i)


# `memoryUsage` after each call (each a list of documents sent as they are)
# into one series of a fresh server: Linux x86-64 mongod 8.2.11 (2026-11-26,
# `tools/probes/linux_bucket_catalog_isolate.py`). The builders' pending
# values, the buffers they keep, the interleaved mode of document values and
# a bucket reopened from the archive all show in the steps.
_LINUX_STEPS: list[tuple[str, list[list], list[int]]] = [
    (
        "a document field, one call",
        [[{"t": _T0, "m": "a", "x": {"a": i}} for i in range(3)]],
        [13236],
    ),
    (
        "a document field, three calls",
        [[{"t": _T0, "m": "a", "x": {"a": i}}] for i in range(3)],
        [13148, 13192, 13236],
    ),
    (
        "an array field, one call",
        [[{"t": _T0, "m": "a", "x": [i, i + 1]} for i in range(3)]],
        [14581],
    ),
    (
        "ten measurements in each of ten calls",
        [
            [{"t": _sec(10 * k + i), "m": "a", "x": (10 * k + i) * 7} for i in range(10)]
            for k in range(10)
        ],
        [13345] + [13154] * 9,
    ),
    (
        "doubles in thirty calls",
        [[{"t": _sec(k), "m": "a", "x": 1.0 + (k * 37 % 11) / 10}] for k in range(30)],
        [13112, 13160, 13208, 13256, 13304, 13352, 13281, 13305, 13329, 13353, 13377, 13401]
        + [13425, 13449, 13473, 13497, 13162, 13210, 13234, 13258, 13282, 13306, 13330, 13354]
        + [13378, 13402, 13426, 13450, 13474, 13498],
    ),
    (
        "strings in six calls",
        [[{"t": _sec(k), "m": "a", "x": f"word{k * k}"}] for k in range(6)],
        [13116, 13188, 13260, 13332, 13285, 13317],
    ),
    (
        "a field missing, then present",
        [
            [{"t": _T0, "m": "a", "x": 1}],
            [{"t": _sec(1), "m": "a"}],
            [{"t": _sec(2), "m": "a", "x": 5}],
        ],
        [13104, 13152, 13200],
    ),
    (
        "a bucket archived for time going backwards, then reopened",
        [
            [{"t": _sec(1800), "m": "a"}],
            [{"t": _sec(-120), "m": "a"}],
            [{"t": _sec(1801), "m": "a"}],
            [{"t": _sec(-121), "m": "a"}],
            [{"t": _sec(1802), "m": "a"}],
        ],
        [11604, 11668, 11692, 11716, 11740],
    ),
    (
        "a document that gains a field",
        [
            [{"t": _T0, "m": "a", "x": {"a": 0}}],
            [{"t": _sec(1), "m": "a", "x": {"a": 0, "b": 1}}],
        ],
        [13148, 15075],
    ),
    (
        "arrays of varying length",
        [[{"t": _sec(k), "m": "a", "x": list(range(k % 5 + 1))}] for k in range(10)],
        [13148, 15075, 15169, 15270, 15378, 15446, 15402, 15460, 15525, 15597],
    ),
    (
        "an array that grows after three measurements",
        [[{"t": _sec(k), "m": "a", "x": [0]}] for k in range(3)]
        + [[{"t": _sec(3), "m": "a", "x": [0, 1]}]],
        [13148, 13216, 13284, 15732],
    ),
    (
        "documents with long strings, twenty calls",
        [
            [{"t": _sec(k), "m": "a", "x": {"a": "s" * 30 + str(k), "b": k * 13, "c": 1.5 * k}}]
            for k in range(20)
        ],
        [14606, 14724, 14842, 14960, 15078, 15196, 15195, 16542, 16606, 16670]
        + [16735, 16799, 16239, 16767, 16783, 16799, 16815, 16831, 16847, 16863],
    ),
    (
        "forty documents in one call",
        [
            [
                {"t": _sec(k), "m": "a", "x": {"a": "word" + str(k * 7), "b": k * 13, "c": 1.5 * k}}
                for k in range(40)
            ]
        ],
        [15685],
    ),
    (
        "nested documents and arrays, three calls",
        [
            [{"t": _sec(k), "m": "a", "x": {"p": {"q": k, "r": "z" * k}, "s": [k, k + 1]}}]
            for k in range(3)
        ],
        [17160, 17274, 17382],
    ),
    (
        "forty fields in a measurement, and fields added over calls",
        [[{"t": _sec(0), "m": "a", **{f"f{i}": i for i in range(40)}}]],
        [58388],
    ),
    (
        "fields added over six calls",
        [[{"t": _sec(k), "m": "a", **{f"f{i}": i for i in range(k * 5 + 1)}}] for k in range(6)],
        [13104, 16164, 21904, 33284, 33368, 33452],
    ),
    (
        "a schema change closing the bucket",
        [
            [{"t": _T0, "m": "a", "x": 1}],
            [{"t": _sec(1), "m": "a", "x": "str"}],
            [{"t": _sec(2), "m": "a", "x": 2}],
        ],
        [13104, 13112, 13104],
    ),
]


@pytest.mark.backlog_fixed("RM-710")
@pytest.mark.parametrize(
    ("calls", "want"),
    [pytest.param(c, w, id=name) for name, c, w in _LINUX_STEPS],
)
def test_bucket_catalog_memory_usage_follows_each_write_in_the_exemplar_bytes(
    tmp_path: pathlib.Path, calls: list[list], want: list[int]
) -> None:
    """`memoryUsage` after each write of a series' measurements is the
    exemplar's (Linux x86-64 mongod): the column builders keep what
    `intermediate()` leaves them, the summaries keep every entry they grew,
    and a bucket that closes or is archived returns what it held (RM-710)."""
    got = []
    with rust_mongo_server(tmp_path) as rust:
        db = rust["rm_bucket_catalog"]
        db.create_collection("ts", timeseries={"timeField": "t", "metaField": "m"})
        for docs in calls:
            db.command("insert", "ts", documents=docs)
            got.append(_section(rust).get("memoryUsage"))
    assert got == want


@pytest.mark.backlog("RM-711")
def test_idle_buckets_expire_past_the_memory_threshold(tmp_path: pathlib.Path) -> None:
    """mongod 8.2.11: once the catalog's memory passes
    `timeseriesIdleBucketExpiryMemoryUsageThreshold` (set to 30000 bytes after
    the first write), opening a bucket for a new metadata value closes idle
    ones: of 21 buckets opened for 21 metadata values the catalog holds fewer
    (14 to 17 across runs: which ones expire depends on the stripe each lands
    in), `numBucketsClosedDueToMemoryThreshold` equals
    `numBucketsArchivedDueToMemoryThreshold` and is at least 1, and every
    bucket document stays stored. Asserted as those facts, not the counts.
    Needs RM-710 (the accounting) to decide when the threshold is passed."""
    t = dt.datetime(2026, 1, 1)

    def probe(client: Any) -> dict:
        coll = _make(client)
        coll.insert_one({"t": t, "m": -1})
        try:
            client.admin.command("setParameter", 1, **{IDLE_THRESHOLD: 30000})
        except pymongo.errors.OperationFailure as e:
            return {"setParameter": ("err", e.code)}
        for i in range(20):
            coll.insert_one({"t": t, "m": i})
        s = _section(client)
        closed = s.get("numBucketsClosedDueToMemoryThreshold", 0)
        archived = s.get("numBucketsArchivedDueToMemoryThreshold", 0)
        return {
            "fewer_buckets_than_opened": s.get("numBuckets", 21) < 21,
            "expired": closed >= 1,
            "closed_equals_archived": closed == archived,
            "all_documents_stored": client["rm_bucket_catalog"][
                "system.buckets.ts"
            ].count_documents({})
            == 21,
        }

    with _fresh_pair(tmp_path) as (rust, ref):
        got, want = probe(rust), probe(ref)
    assert want == {
        "fewer_buckets_than_opened": True,
        "expired": True,
        "closed_equals_archived": True,
        "all_documents_stored": True,
    }
    assert got == want


def _split(client: Any) -> list:
    s = _section(client)
    return [s.get(k) for k in ("numBuckets", "numOpenBuckets", "numArchivedBuckets")] + [
        s.get("stateManagement", {}).get("bucketsManaged")
    ]


@pytest.mark.backlog("RM-798")
def test_open_and_archived_buckets_split_as_mongod_counts_them(tmp_path: pathlib.Path) -> None:
    """mongod 8.2.11 (measured 2026-11-25): a measurement for metadata 0 at
    `t`, after that series' bucket rolled forward to `t + 2 days`, leaves
    `numBuckets` 5 as here, but `numOpenBuckets` 3 and `numArchivedBuckets`
    2 (`bucketsManaged` 4); this server counts 4 open and 1 archived."""
    t = dt.datetime(2026, 1, 1)

    def probe(client: Any) -> list:
        coll = _make(client)
        coll.insert_many([{"t": t, "m": i % 2} for i in range(4)])
        coll.insert_one({"t": t, "m": 0})
        coll.insert_one({"t": t + dt.timedelta(days=2), "m": 0})
        coll.insert_one({"t": t - dt.timedelta(days=2), "m": 1})
        coll.insert_many([{"t": t, "m": 5, "v": i} for i in range(1005)])
        coll.insert_one({"t": t, "m": 0, "v": "str"})
        return _split(client)

    with _fresh_pair(tmp_path) as (rust, ref):
        got, want = probe(rust), probe(ref)
    assert want == [5, 3, 2, 4]
    assert got == want


@pytest.mark.backlog("RM-799")
def test_dropping_a_collection_clears_its_buckets_by_era(tmp_path: pathlib.Path) -> None:
    """mongod 8.2.11 (measured 2026-11-25) clears a dropped collection's
    buckets lazily, by era: after two time-series collections hold a bucket
    each and one is dropped, `numBuckets` is 1 but `numOpenBuckets` stays 2
    (`numArchivedBuckets` -1), and `stateManagement` reads `bucketsManaged`
    2, `currentEra` 1, `trackedClearOperations` 1. This server forgets the
    buckets at once and keeps no eras."""
    t = dt.datetime(2026, 1, 1)

    def probe(client: Any) -> list:
        db = client["rm_bucket_catalog"]
        for name in ("a", "b"):
            db.create_collection(name, timeseries={"timeField": "t", "metaField": "m"})
            db[name].insert_one({"t": t, "m": 0})
        db.a.drop()
        s = _section(client)
        return _split(client) + [
            s["stateManagement"]["currentEra"],
            s["stateManagement"]["trackedClearOperations"],
        ]

    with _fresh_pair(tmp_path) as (rust, ref):
        got, want = probe(rust), probe(ref)
    assert want == [1, 2, -1, 2, 1, 1]
    assert got == want
