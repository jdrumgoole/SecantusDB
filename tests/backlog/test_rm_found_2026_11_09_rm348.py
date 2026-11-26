"""Gap tests for what was found on 2026-11-09 while reading a time-series
`aggregate` bucket by bucket (RM-348): the time-series reads that still
read the collection at the command.

Each reads 4,000 measurements in 50 series, 100 buckets (50 of 72 in the
first hour, 50 of 8 after it), takes one, deletes every measurement, and
counts what the cursor still returns: what mongod's read had buffered.

The reference is mongod 8.2.11 (`mongod`); the Rust server is
`secantusd-mdb` (`rust_mongo`).
"""

from __future__ import annotations

import datetime
import uuid
from collections.abc import Callable
from typing import Any

import pytest


def _after_delete(client: Any, read: Callable[[Any], Any]) -> int:
    db = client["rm_found_1109_rm348"]
    name = "ts" + uuid.uuid4().hex[:8]
    db.create_collection(name, timeseries={"timeField": "t", "metaField": "m"})
    c = db[name]
    base = datetime.datetime(2024, 1, 1)
    c.insert_many(
        [{"t": base + datetime.timedelta(seconds=i), "m": i % 50, "v": i} for i in range(4000)]
    )
    cur = read(c)
    next(cur)
    c.delete_many({"m": {"$gte": 0}})
    return len(list(cur))


@pytest.mark.backlog_fixed("RM-455")
def test_a_time_series_find_goes_bucket_by_bucket(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 answers a `find` of a time-series collection with an
    aggregation whose cursor stage buffers 32 buckets first: 2,303 more
    after the delete. This server read the collection at the command, and
    returned 3,999, until its `find` became that aggregation too."""

    def read(c: Any) -> Any:
        return c.find({"v": {"$gte": 0}}).batch_size(1)

    want = _after_delete(mongod, read)
    assert want == 2303
    assert _after_delete(rust_mongo, read) == want


@pytest.mark.backlog_fixed("RM-456")
def test_a_time_series_projection_reads_a_bucket_at_a_time(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 reads a time-series `aggregate` behind an inclusion
    `$project` one bucket at a time: the rest of the first bucket, 71.
    This server read it whole, 3,999, until such a pipeline read its
    buckets one at a time with no cursor-stage buffer."""

    def read(c: Any) -> Any:
        return c.aggregate([{"$match": {"v": {"$gte": 0}}}, {"$project": {"v": 1}}], batchSize=1)

    want = _after_delete(mongod, read)
    assert want == 71
    assert _after_delete(rust_mongo, read) == want


@pytest.mark.backlog_fixed("RM-457")
def test_a_time_series_read_of_the_meta_index_buffers_its_buckets(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 reads a filter on the meta field from the default
    `m_1_t_1` index, buffering 32 buckets series by series: 1,279. This
    server read it whole, 3,199, until that index was built over the
    buckets and the read scanned it."""

    def read(c: Any) -> Any:
        return c.aggregate([{"$match": {"m": {"$lt": 40}}}], batchSize=1)

    want = _after_delete(mongod, read)
    assert want == 1279
    assert _after_delete(rust_mongo, read) == want


@pytest.mark.backlog_fixed("RM-701")
def test_a_hinted_time_series_read_scans_the_index_over_the_buckets(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 reads a time-series `aggregate` hinted to the default
    `m_1_t_1` index, by name or by key, by scanning that index over the
    buckets, buffering 32 of them series by series: 1,279. `{$natural: 1}`
    reads as no hint does: 2,303. This server read a hinted read whole,
    3,999, until a hint to the default index scanned it."""

    for hint, want in (("m_1_t_1", 1279), ({"m": 1, "t": 1}, 1279), ({"$natural": 1}, 2303)):

        def read(c: Any, hint: Any = hint) -> Any:
            return c.aggregate([{"$match": {"v": {"$gte": 0}}}], batchSize=1, hint=hint)

        assert _after_delete(mongod, read) == want
        assert _after_delete(rust_mongo, read) == want


@pytest.mark.backlog_fixed("RM-701")
def test_a_time_series_read_under_a_collation_buffers_its_buckets(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 reads a time-series `aggregate` under a collation
    through its cursor stage, 32 buckets first: 2,303 after the delete.
    This server read it whole, 3,999, until a collated read with no string
    bound streamed its buckets."""

    def read(c: Any) -> Any:
        return c.aggregate(
            [{"$match": {"v": {"$gte": 0}}}], batchSize=1, collation={"locale": "en"}
        )

    want = _after_delete(mongod, read)
    assert want == 2303
    assert _after_delete(rust_mongo, read) == want


@pytest.mark.backlog_fixed("RM-701")
def test_a_time_series_read_under_a_collation_matches_as_mongod(
    rust_mongo: Any, mongod: Any
) -> None:
    """A time-series read under a case-insensitive collation answers as
    mongod 8.2.11 does: a string bound matches under the collation, with
    no bucket pruned on its binary `control.min` / `control.max`."""

    def run(client: Any) -> Any:
        db = client["rm_found_1109_rm348"]
        name = "ts" + uuid.uuid4().hex[:8]
        db.create_collection(name, timeseries={"timeField": "t", "metaField": "m"})
        c = db[name]
        base = datetime.datetime(2024, 1, 1)
        c.insert_many(
            [
                {"t": base + datetime.timedelta(hours=i), "m": i % 3, "s": s, "v": i}
                for i, s in enumerate(["abc", "ABC", "Abd", "b", "B", "a"])
            ]
        )
        out = []
        for match in ({"s": "abc"}, {"s": {"$gte": "B"}}, {"v": {"$gte": 2}}):
            got = c.aggregate([{"$match": match}], collation={"locale": "en", "strength": 2})
            out.append(sorted(d["v"] for d in got))
        return out

    assert run(rust_mongo) == run(mongod)


@pytest.mark.backlog_fixed("RM-701")
def test_a_time_series_read_beside_an_index_of_its_own_buffers_its_buckets(
    rust_mongo: Any, mongod: Any
) -> None:
    """A time-series collection with an index of its own, `{v: 1}`: mongod
    8.2.11 still reads an unindexed filter's buckets through its cursor
    stage, 32 buckets first: 2,303 after the delete. This server read the
    collection whole at the command, 3,999, until a filter the index
    cannot serve streamed its buckets."""

    def read(c: Any) -> Any:
        c.create_index("v")
        return c.aggregate([{"$match": {"x": {"$exists": False}}}], batchSize=1)

    want = _after_delete(mongod, read)
    assert want == 2303
    assert _after_delete(rust_mongo, read) == want


@pytest.mark.backlog_fixed("RM-701")
def test_a_time_series_read_beside_an_index_of_its_own_matches_as_mongod(
    rust_mongo: Any, mongod: Any
) -> None:
    """With `{v: 1}` beside the default index, a read on `v`, on the meta
    field and on neither answers as mongod 8.2.11 does."""

    def run(client: Any) -> Any:
        db = client["rm_found_1109_rm348"]
        name = "ts" + uuid.uuid4().hex[:8]
        db.create_collection(name, timeseries={"timeField": "t", "metaField": "m"})
        c = db[name]
        base = datetime.datetime(2024, 1, 1)
        c.insert_many(
            [
                {"t": base + datetime.timedelta(minutes=i), "m": i % 5, "v": i, "w": i % 7}
                for i in range(300)
            ]
        )
        c.create_index("v")
        out = []
        for match in ({"v": {"$gte": 250}}, {"m": 2, "w": 3}, {"w": {"$lt": 2}}):
            out.append(sorted(d["v"] for d in c.aggregate([{"$match": match}])))
        return out

    assert run(rust_mongo) == run(mongod)


@pytest.mark.backlog_fixed("RM-701")
def test_a_time_series_read_of_its_own_index_buffers_as_mongod(
    rust_mongo: Any, mongod: Any
) -> None:
    """With `{v: 1}` beside the default index, a filter on `v` that every
    measurement passes: mongod 8.2.11's planner picks the buckets in order
    over its index, 32 buckets first, 2,303 after the delete; this server
    reads any filter naming an own index's field whole, 3,999."""

    def read(c: Any) -> Any:
        c.create_index("v")
        return c.aggregate([{"$match": {"v": {"$gte": 0}}}], batchSize=1)

    want = _after_delete(mongod, read)
    assert want == 2303
    assert _after_delete(rust_mongo, read) == want


def _own_index(*indexes: Any, **options: Any) -> Callable[[Any, Any], None]:
    """A setup step that gives the collection the indexes named by
    `create_index` arguments, and `drop` / `hint` extras are left to the
    reads themselves."""

    def setup(db: Any, c: Any) -> None:
        for spec in indexes:
            c.create_index(spec, **options)

    return setup


def _after_delete_setup(client: Any, setup: Callable[[Any, Any], None], read: Any) -> int:
    db = client["rm_found_1109_rm348"]
    name = "ts" + uuid.uuid4().hex[:8]
    db.create_collection(name, timeseries={"timeField": "t", "metaField": "m"})
    c = db[name]
    base = datetime.datetime(2024, 1, 1)
    c.insert_many(
        [
            {"t": base + datetime.timedelta(seconds=i), "m": i % 50, "v": i, "w": (i * 7) % 4000}
            for i in range(4000)
        ]
    )
    setup(db, c)
    cur = read(c)
    next(cur)
    c.delete_many({"m": {"$gte": 0}})
    return len(list(cur))


@pytest.mark.backlog_fixed("RM-701")
def test_a_selective_filter_scans_the_index_over_the_buckets(rust_mongo: Any, mongod: Any) -> None:
    """With `{v: 1}` beside the default index, a filter that bounds it
    (`v < 100`, the buckets by `control.min.v`) is an IXSCAN of the index
    over the buckets to mongod 8.2.11's planner, not the buckets in order:
    63 measurements are left after the delete. A bound on `control.max.v`
    alone (`v >= n`) is a collection scan, the buckets in order. This server
    read both whole, 99, until it kept the index over the buckets and
    planned over it."""

    def run(client: Any) -> list[int]:
        out = []
        for f in ({"v": {"$lt": 100}}, {"v": {"$lte": 3999}}, {"v": {"$gte": 0}}):
            out.append(
                _after_delete_setup(
                    client,
                    _own_index("v"),
                    lambda c, f=f: c.aggregate([{"$match": f}], batchSize=1),
                )
            )
        return out

    want = run(mongod)
    assert want[:1] == [63]
    assert run(rust_mongo) == want


@pytest.mark.backlog_fixed("RM-701")
def test_a_hint_to_an_index_of_its_own_scans_it_over_the_buckets(
    rust_mongo: Any, mongod: Any
) -> None:
    """A hint to `{v: 1}`, by key or by name, is mongod 8.2.11's scan of
    that index over the buckets whatever the filter (2,303 after the
    delete here, the buckets in `{control.min.v, control.max.v}` order);
    this server read the collection whole, 3,999."""

    def run(client: Any) -> list[int]:
        out = []
        for hint in ({"v": 1}, "v_1"):
            out.append(
                _after_delete_setup(
                    client,
                    _own_index("v"),
                    lambda c, hint=hint: c.aggregate(
                        [{"$match": {"w": {"$gte": 0}}}], hint=hint, batchSize=1
                    ),
                )
            )
        return out

    assert run(rust_mongo) == run(mongod)


@pytest.mark.backlog_fixed("RM-701")
def test_a_hinted_read_of_no_filter_names_the_index_over_the_buckets(
    rust_mongo: Any, mongod: Any
) -> None:
    """The profile entry of a hinted `find` or `aggregate` that filters
    nothing names the index it scanned as the buckets keep it
    (`IXSCAN { control.min.v: 1, control.max.v: 1 }`); this server's
    `planSummary` of an aggregate's hint was `COLLSCAN`, as it ignored the
    hint."""

    def run(client: Any) -> list[Any]:
        db = client["rm_found_1109_rm348_summary"]
        client.drop_database(db.name)
        db.create_collection("ts", timeseries={"timeField": "t", "metaField": "m"})
        base = datetime.datetime(2024, 1, 1)
        db.ts.insert_many(
            [{"t": base + datetime.timedelta(seconds=i), "m": i % 3, "v": i} for i in range(30)]
        )
        db.ts.create_index("v")
        db.command("profile", 2)
        list(db.ts.find({}).hint("m_1_t_1"))
        list(db.ts.find({}).hint("v_1"))
        list(db.ts.aggregate([{"$match": {}}], hint="v_1"))
        db.command("profile", 0)
        out = [
            e.get("planSummary")
            for e in db.system.profile.find({"ns": db.name + ".ts"}).sort("ts", 1)
            if "planSummary" in e
        ]
        client.drop_database(db.name)
        return out

    want = run(mongod)
    assert want[1] == "IXSCAN { control.min.v: 1, control.max.v: 1 }"
    assert run(rust_mongo) == want


@pytest.mark.backlog_fixed("RM-701")
def test_dropping_the_default_index_leaves_no_copy_over_the_buckets(
    rust_mongo: Any, mongod: Any
) -> None:
    """With `m_1_t_1` dropped, a filter on the meta field is the buckets in
    order for mongod 8.2.11 (2,303 after the delete); this server kept the
    index's copy over the buckets and read the collection whole, 3,999."""

    def drop(db: Any, c: Any) -> None:
        db.command("dropIndexes", c.name, index="m_1_t_1")

    def run(client: Any) -> int:
        return _after_delete_setup(
            client,
            drop,
            lambda c: c.aggregate([{"$match": {"m": {"$gte": 0}}}], batchSize=1),
        )

    assert run(rust_mongo) == run(mongod)


@pytest.mark.backlog_fixed("RM-701")
def test_an_index_of_its_own_is_sized_and_dropped_over_the_buckets(
    rust_mongo: Any, mongod: Any
) -> None:
    """`createIndexes` builds the index over the buckets, so `collStats`
    sizes it (mongod 8.2.11: every index holds pages), and `dropIndexes`
    takes its copy away with the collection's."""

    def run(client: Any) -> Any:
        db = client["rm_found_1109_rm348"]
        name = "ts" + uuid.uuid4().hex[:8]
        db.create_collection(name, timeseries={"timeField": "t", "metaField": "m"})
        c = db[name]
        base = datetime.datetime(2024, 1, 1)
        c.insert_many(
            [{"t": base + datetime.timedelta(seconds=i), "m": i % 5, "v": i} for i in range(500)]
        )
        c.create_index("v")
        c.create_index([("m", 1), ("v", -1)])
        sized = {k: v > 0 for k, v in db.command("collStats", name)["indexSizes"].items()}
        db.command("dropIndexes", name, index="v_1")
        db.command("dropIndexes", name, index="m_1_t_1")
        return (
            sized,
            sorted(db.command("collStats", name)["indexSizes"]),
            sorted(i["name"] for i in db["system.buckets." + name].list_indexes()),
        )

    assert run(rust_mongo) == run(mongod)


@pytest.mark.backlog("RM-701")
def test_a_time_series_read_beside_a_partial_index_scans_it_as_mongod(
    rust_mongo: Any, mongod: Any
) -> None:
    """With a partial index `{v: 1}` (`v > 5`) a filter that implies its
    filter is planned over the index over the buckets, which is not a scan
    of the buckets in order: mongod 8.2.11 leaves 1,913 after the delete;
    this server keeps no copy of a partial index over the buckets and
    reads the collection whole."""

    def read(c: Any) -> Any:
        return c.aggregate([{"$match": {"v": {"$lt": 3000, "$gt": 5}}}], batchSize=1)

    def setup(db: Any, c: Any) -> None:
        c.create_index("v", partialFilterExpression={"v": {"$gt": 5}})

    want = _after_delete_setup(mongod, setup, read)
    assert want == 1913
    assert _after_delete_setup(rust_mongo, setup, read) == want


@pytest.mark.backlog("RM-701")
def test_a_time_series_expr_read_beside_an_index_of_its_own_buffers_as_mongod(
    rust_mongo: Any, mongod: Any
) -> None:
    """With `{v: 1}` beside the default index, an `$expr` filter reads the
    buckets in order through mongod 8.2.11's cursor stage (2,303 after the
    delete); this server reads any `$expr` / `$text` / `$where` filter whole
    when the collection has an index of its own."""

    def read(c: Any) -> Any:
        return c.aggregate([{"$match": {"$expr": {"$gte": ["$v", 0]}}}], batchSize=1)

    want = _after_delete_setup(mongod, _own_index("v"), read)
    assert want == 2303
    assert _after_delete_setup(rust_mongo, _own_index("v"), read) == want


@pytest.mark.backlog("RM-810")
def test_a_time_series_meta_and_time_read_is_tried_against_the_clustered_scan(
    rust_mongo: Any, mongod: Any
) -> None:
    """With only the default `m_1_t_1`, a filter on the meta and the time
    fields has two plans for mongod 8.2.11 (the index scan and the clustered
    scan of the bucket `_id`s) and runs a trial between them: 2,399
    measurements are left after the delete. This server reads through the
    default index alone, 1,535."""

    def read(c: Any) -> Any:
        cutoff = datetime.datetime(2024, 1, 1, 0, 40)
        return c.aggregate([{"$match": {"m": {"$gte": 0}, "t": {"$lt": cutoff}}}], batchSize=1)

    def nothing(db: Any, c: Any) -> None:
        return None

    want = _after_delete_setup(mongod, nothing, read)
    assert want == 2399
    assert _after_delete_setup(rust_mongo, nothing, read) == want
