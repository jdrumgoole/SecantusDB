"""Differential-probe sessions and multi-document transactions against mongod.

Each scenario is a short script of commands sent over one or more raw
connections, with the session id, transaction number and transaction flags
written out in full, so the probe controls exactly what a driver would send
and can also send what a driver never would. Every reply is compared with
mongod's. A fresh database per scenario.

    PROBE_MONGOD=mongodb://127.0.0.1:27017 \
    PROBE_SERVER=mongodb://127.0.0.1:27018 python tools/probes/transactions.py

mongod must be a replica-set member: a standalone refuses transactions.
`PROBE_ONLY=<substring>` runs the scenarios whose name contains it, and
`PROBE_VERBOSE=1` prints both replies for every difference.
"""

import os
import re
import sys
import uuid

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from _wire import Wire  # noqa: E402
from bson import Binary, Int64  # noqa: E402

DEFAULT_MONGOD = "mongodb://127.0.0.1:27041"
NOISE = ("$clusterTime", "operationTime", "opTime", "electionId", "localTime")


class Sess:
    """A session id, made the same on both servers from the scenario's name."""

    def __init__(self, label):
        self.label = label

    def __repr__(self):
        return f"<session {self.label}>"


class Cur:
    """The id of a cursor an earlier step saved under `label`."""

    def __init__(self, label):
        self.label = label

    def __repr__(self):
        return f"<cursor {self.label}>"


# ---------------------------------------------------------------- helpers


def lsid(label):
    return {"id": Sess(label)}


def tx(sess, n, command, start=False, **more):
    out = {**command, "lsid": lsid(sess), "txnNumber": Int64(n), "autocommit": False}
    if start:
        out["startTransaction"] = True
    out.update(more)
    return out


def in_sess(sess, command, **more):
    return {**command, "lsid": lsid(sess), **more}


def retry(sess, n, command, **more):
    return {**command, "lsid": lsid(sess), "txnNumber": Int64(n), **more}


def commit(sess, n, conn="a", **more):
    return (conn, "admin", tx(sess, n, {"commitTransaction": 1}, **more))


def abort(sess, n, conn="a", **more):
    return (conn, "admin", tx(sess, n, {"abortTransaction": 1}, **more))


def insert(name, *docs, **more):
    return {"insert": name, "documents": list(docs), **more}


def find(name, filter_=None, **more):
    return {"find": name, "filter": filter_ or {}, **more}


def agg(name, *pipeline, **more):
    return {"aggregate": name, "pipeline": list(pipeline), "cursor": {}, **more}


def update(name, q, u, **more):
    return {"update": name, "updates": [{"q": q, "u": u, **more}]}


def delete(name, q, limit=0):
    return {"delete": name, "deletes": [{"q": q, "limit": limit}]}


def fam(name, q, **more):
    return {"findAndModify": name, "query": q, **more}


SEED = insert("c", {"_id": 1, "v": 1}, {"_id": 2, "v": 2})
ALL = find("c", sort={"_id": 1})
SET9 = {"$set": {"v": 9}}

S: list[tuple[str, list]] = []


def scenario(name, *steps):
    S.append((name, list(steps)))


# ------------------------------------------------------------- the basics

scenario(
    "self-check: commit makes the write visible",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    commit("s", 1),
    ALL,
)
scenario(
    "abort discards the write",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    abort("s", 1),
    ALL,
)
scenario(
    "a transaction reads its own writes",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    tx("s", 1, ALL),
    tx("s", 1, update("c", {"_id": 1}, SET9)),
    tx("s", 1, ALL),
    tx("s", 1, delete("c", {"_id": 2}, 1)),
    tx("s", 1, ALL),
    commit("s", 1),
    ALL,
)
scenario(
    "nobody else sees an uncommitted write",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    tx("s", 1, update("c", {"_id": 1}, SET9)),
    ("b", None, ALL),
    ("b", None, {"count": "c"}),
    commit("s", 1),
    ("b", None, ALL),
)
scenario(
    "a transaction does not see a later commit",
    SEED,
    tx("s", 1, ALL, start=True),
    ("b", None, insert("c", {"_id": 3})),
    ("b", None, update("c", {"_id": 2}, SET9)),
    tx("s", 1, ALL),
    tx("s", 1, agg("c", {"$group": {"_id": None, "n": {"$sum": 1}}})),
    commit("s", 1),
    ALL,
)
scenario(
    "a transaction that only reads commits",
    SEED,
    tx("s", 1, ALL, start=True),
    commit("s", 1),
)
scenario(
    "a transaction that only reads aborts",
    SEED,
    tx("s", 1, ALL, start=True),
    abort("s", 1),
)
scenario(
    "two transactions on one session, one after the other",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    commit("s", 1),
    tx("s", 2, insert("c", {"_id": 4}), start=True),
    commit("s", 2),
    ALL,
)
scenario(
    "a transaction number may skip ahead",
    SEED,
    tx("s", 5, insert("c", {"_id": 3}), start=True),
    commit("s", 5),
    tx("s", 50, insert("c", {"_id": 4}), start=True),
    commit("s", 50),
    ALL,
)
scenario(
    "two sessions, each with its own transaction",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    ("b", None, tx("t", 1, insert("c", {"_id": 4}), start=True)),
    tx("s", 1, ALL),
    ("b", None, tx("t", 1, ALL)),
    commit("s", 1),
    ("b", "admin", tx("t", 1, {"commitTransaction": 1})),
    ALL,
)
scenario(
    "one session used from two connections",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    ("b", None, tx("s", 1, insert("c", {"_id": 4}))),
    ("b", None, tx("s", 1, ALL)),
    commit("s", 1, conn="b"),
    ALL,
)
scenario(
    "writes to two collections and two databases",
    SEED,
    insert("d", {"_id": 1}),
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    tx("s", 1, insert("d", {"_id": 2})),
    ("a", "@other", tx("s", 1, insert("e", {"_id": 1}))),
    commit("s", 1),
    ALL,
    find("d", sort={"_id": 1}),
    ("a", "@other", find("e")),
)
scenario(
    "every write command inside one transaction",
    SEED,
    tx("s", 1, update("c", {"_id": 1}, {"$inc": {"v": 10}}), start=True),
    tx("s", 1, update("c", {}, {"$set": {"m": 1}}, multi=True)),
    tx("s", 1, update("c", {"_id": 7}, {"$set": {"v": 7}}, upsert=True)),
    tx("s", 1, fam("c", {"_id": 2}, update={"$inc": {"v": 1}}, new=True)),
    tx("s", 1, fam("c", {"_id": 1}, remove=True)),
    tx("s", 1, fam("c", {"_id": 8}, update={"$set": {"v": 8}}, upsert=True, new=True)),
    tx("s", 1, delete("c", {"v": {"$gte": 8}})),
    tx("s", 1, ALL),
    commit("s", 1),
    ALL,
)
scenario(
    "every read command inside one transaction",
    SEED,
    tx("s", 1, ALL, start=True),
    tx("s", 1, agg("c", {"$match": {"v": {"$gte": 2}}})),
    tx("s", 1, {"distinct": "c", "key": "v"}),
    tx("s", 1, agg("c", {"$count": "n"})),
    tx("s", 1, find("c", {"_id": 1}, projection={"v": 1, "_id": 0})),
    tx(
        "s",
        1,
        agg("c", {"$lookup": {"from": "c", "localField": "_id", "foreignField": "v", "as": "j"}}),
    ),
    commit("s", 1),
)

# -------------------------------------------------- the transaction flags

scenario("commit with no transaction", SEED, commit("s", 1))
scenario("abort with no transaction", SEED, abort("s", 1))
scenario(
    "a statement with no startTransaction",
    SEED,
    tx("s", 1, insert("c", {"_id": 3})),
    ALL,
)
scenario(
    "startTransaction twice with one number",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    tx("s", 1, insert("c", {"_id": 4}), start=True),
    commit("s", 1),
    ALL,
)
scenario(
    "a new transaction while one is open aborts the old one",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    tx("s", 2, insert("c", {"_id": 4}), start=True),
    commit("s", 1),
    commit("s", 2),
    ALL,
)
scenario(
    "an older transaction number",
    SEED,
    tx("s", 5, insert("c", {"_id": 3}), start=True),
    commit("s", 5),
    tx("s", 4, insert("c", {"_id": 4}), start=True),
    tx("s", 4, ALL),
    commit("s", 4),
    abort("s", 4),
    ALL,
)
scenario(
    "a statement after commit",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    commit("s", 1),
    tx("s", 1, insert("c", {"_id": 4})),
    tx("s", 1, ALL),
    ALL,
)
scenario(
    "a statement after abort",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    abort("s", 1),
    tx("s", 1, insert("c", {"_id": 4})),
    tx("s", 1, ALL),
    ALL,
)
scenario(
    "commit twice",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    commit("s", 1),
    commit("s", 1),
    ALL,
)
scenario(
    "abort twice",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    abort("s", 1),
    abort("s", 1),
)
scenario(
    "abort after commit",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    commit("s", 1),
    abort("s", 1),
    ALL,
)
scenario(
    "commit after abort",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    abort("s", 1),
    commit("s", 1),
    ALL,
)
scenario(
    "startTransaction with a committed number",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    commit("s", 1),
    tx("s", 1, insert("c", {"_id": 4}), start=True),
    ALL,
)
scenario(
    "startTransaction with an aborted number",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    abort("s", 1),
    tx("s", 1, insert("c", {"_id": 4}), start=True),
    commit("s", 1),
    ALL,
)
scenario(
    "autocommit true",
    SEED,
    {**insert("c", {"_id": 3}), "lsid": lsid("s"), "txnNumber": Int64(1), "autocommit": True},
    ALL,
)
scenario(
    "autocommit true with startTransaction",
    SEED,
    {
        **insert("c", {"_id": 3}),
        "lsid": lsid("s"),
        "txnNumber": Int64(1),
        "autocommit": True,
        "startTransaction": True,
    },
)
scenario(
    "startTransaction false",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), startTransaction=False),
)
scenario(
    "startTransaction without autocommit",
    SEED,
    {**insert("c", {"_id": 3}), "lsid": lsid("s"), "txnNumber": Int64(1), "startTransaction": True},
    ALL,
)
scenario(
    "autocommit without a transaction number",
    SEED,
    {**insert("c", {"_id": 3}), "lsid": lsid("s"), "autocommit": False},
    {**insert("c", {"_id": 4}), "lsid": lsid("s"), "autocommit": False, "startTransaction": True},
    {**insert("c", {"_id": 5}), "autocommit": False},
    {**insert("c", {"_id": 6}), "startTransaction": True},
    ALL,
)
scenario(
    "a transaction number with no session",
    SEED,
    {**insert("c", {"_id": 3}), "txnNumber": Int64(1)},
    {
        **insert("c", {"_id": 4}),
        "txnNumber": Int64(1),
        "autocommit": False,
        "startTransaction": True,
    },
    {**find("c"), "txnNumber": Int64(1)},
    ALL,
)
scenario(
    "transaction numbers of the wrong type or sign",
    SEED,
    {
        **insert("c", {"_id": 3}),
        "lsid": lsid("s"),
        "txnNumber": 1,
        "autocommit": False,
        "startTransaction": True,
    },
    {
        **insert("c", {"_id": 4}),
        "lsid": lsid("t"),
        "txnNumber": 1.0,
        "autocommit": False,
        "startTransaction": True,
    },
    {
        **insert("c", {"_id": 5}),
        "lsid": lsid("u"),
        "txnNumber": "1",
        "autocommit": False,
        "startTransaction": True,
    },
    tx("v", -1, insert("c", {"_id": 6}), start=True),
    tx("w", 0, insert("c", {"_id": 7}), start=True),
    commit("w", 0),
    {
        **insert("c", {"_id": 8}),
        "lsid": lsid("x"),
        "txnNumber": Int64(1),
        "autocommit": 0,
        "startTransaction": True,
    },
    {
        **insert("c", {"_id": 9}),
        "lsid": lsid("y"),
        "txnNumber": Int64(1),
        "autocommit": False,
        "startTransaction": 1,
    },
    ALL,
)
scenario(
    "session ids of the wrong shape",
    SEED,
    {**find("c"), "lsid": 1},
    {**find("c"), "lsid": {}},
    {**find("c"), "lsid": {"id": 1}},
    {**find("c"), "lsid": {"id": "abc"}},
    {**find("c"), "lsid": {"id": Binary(b"0123456789abcdef", 0)}},
    {**find("c"), "lsid": {"id": Binary(b"short", 4)}},
    {**find("c"), "lsid": {"id": Sess("s"), "extra": 1}},
    {**find("c"), "lsid": {"id": Sess("s"), "uid": Binary(b"\x00" * 32, 0)}},
    {**find("c"), "lsid": {"id": Sess("s"), "txnNumber": Int64(1)}},
    {**find("c", limit=1), "lsid": lsid("s")},
)
scenario(
    "commit and abort outside admin",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    ("a", None, tx("s", 1, {"commitTransaction": 1})),
    ("a", None, tx("s", 1, {"abortTransaction": 1})),
    tx("s", 1, ALL),
    commit("s", 1),
    ALL,
)
scenario(
    "commit and abort with odd arguments",
    SEED,
    ("a", "admin", {"commitTransaction": 1}),
    ("a", "admin", {"abortTransaction": 1}),
    ("a", "admin", {"commitTransaction": 1, "lsid": lsid("s")}),
    ("a", "admin", {"abortTransaction": 1, "lsid": lsid("s")}),
    ("a", "admin", {"commitTransaction": 1, "lsid": lsid("s"), "txnNumber": Int64(1)}),
    ("a", "admin", {"abortTransaction": 1, "lsid": lsid("s"), "txnNumber": Int64(1)}),
    tx("s", 2, insert("c", {"_id": 3}), start=True),
    ("a", "admin", tx("s", 2, {"commitTransaction": 1}, start=True)),
    ("a", "admin", tx("s", 2, {"commitTransaction": 1, "bogus": 1})),
    ("a", "admin", tx("s", 2, {"commitTransaction": "x"})),
    ("a", "admin", tx("s", 2, {"commitTransaction": 1, "writeConcern": {"w": "majority"}})),
    ALL,
)
scenario(
    "commit as the first statement",
    SEED,
    ("a", "admin", tx("s", 1, {"commitTransaction": 1}, start=True)),
    ("a", "admin", tx("t", 1, {"abortTransaction": 1}, start=True)),
)

# ------------------------------------------ read concern and write concern

scenario(
    "read concern on the first statement",
    SEED,
    tx("s", 1, ALL, start=True, readConcern={"level": "snapshot"}),
    commit("s", 1),
    tx("s", 2, ALL, start=True, readConcern={"level": "majority"}),
    commit("s", 2),
    tx("s", 3, ALL, start=True, readConcern={"level": "local"}),
    commit("s", 3),
    tx("s", 4, ALL, start=True, readConcern={}),
    commit("s", 4),
)
scenario(
    "read concern levels a transaction refuses",
    SEED,
    tx("s", 1, ALL, start=True, readConcern={"level": "linearizable"}),
    tx("s", 2, ALL, start=True, readConcern={"level": "available"}),
    tx("s", 3, ALL, start=True, readConcern={"level": "bogus"}),
    tx("s", 4, ALL, start=True, readConcern="snapshot"),
    tx("s", 5, ALL, start=True, readConcern={"level": 1}),
    tx("s", 6, ALL, start=True, readConcern={"level": "snapshot", "bogus": 1}),
)
scenario(
    "read concern on a later statement",
    SEED,
    tx("s", 1, ALL, start=True),
    tx("s", 1, ALL, readConcern={"level": "snapshot"}),
    tx("s", 1, ALL),
    commit("s", 1),
)
scenario(
    "an empty read concern on a later statement",
    SEED,
    tx("s", 1, ALL, start=True),
    tx("s", 1, ALL, readConcern={}),
    commit("s", 1),
)
scenario(
    "write concern on a statement",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}, writeConcern={"w": 1}), start=True),
    tx("s", 2, insert("c", {"_id": 4}), start=True),
    tx("s", 2, insert("c", {"_id": 5}, writeConcern={"w": "majority"})),
    tx("s", 2, ALL),
    commit("s", 2),
    ALL,
)
scenario(
    "write concern on commit and abort",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    commit("s", 1, writeConcern={"w": 1}),
    tx("s", 2, insert("c", {"_id": 4}), start=True),
    commit("s", 2, writeConcern={"w": "majority", "wtimeout": 1000}),
    tx("s", 3, insert("c", {"_id": 5}), start=True),
    abort("s", 3, writeConcern={"w": "majority"}),
    tx("s", 4, insert("c", {"_id": 6}), start=True),
    commit("s", 4, writeConcern={"w": 0}),
    ALL,
)
scenario(
    "read preference in a transaction",
    SEED,
    tx("s", 1, {**ALL, "$readPreference": {"mode": "primary"}}, start=True),
    commit("s", 1),
    tx("s", 2, {**ALL, "$readPreference": {"mode": "secondary"}}, start=True),
    tx("s", 2, ALL),
    tx("s", 3, {**ALL, "$readPreference": {"mode": "primaryPreferred"}}, start=True),
)
scenario(
    "snapshot read concern outside a transaction",
    SEED,
    {**ALL, "readConcern": {"level": "snapshot"}},
    {**agg("c", {"$sort": {"_id": 1}}), "readConcern": {"level": "snapshot"}},
    {"distinct": "c", "key": "v", "readConcern": {"level": "snapshot"}},
    {**insert("c", {"_id": 3}), "readConcern": {"level": "snapshot"}},
    {"count": "c", "readConcern": {"level": "snapshot"}},
)

# ---------------------------------------------------------------- conflicts

scenario(
    "two transactions update one document",
    SEED,
    tx("s", 1, update("c", {"_id": 1}, SET9), start=True),
    ("b", None, tx("t", 1, update("c", {"_id": 1}, {"$set": {"v": 8}}), start=True)),
    ("b", None, tx("t", 1, ALL)),
    ("b", "admin", tx("t", 1, {"commitTransaction": 1})),
    commit("s", 1),
    ALL,
)
scenario(
    "two transactions insert one _id",
    SEED,
    tx("s", 1, insert("c", {"_id": 3, "by": "s"}), start=True),
    ("b", None, tx("t", 1, insert("c", {"_id": 3, "by": "t"}), start=True)),
    ("b", "admin", tx("t", 1, {"abortTransaction": 1})),
    commit("s", 1),
    ALL,
)
scenario(
    "two transactions delete one document",
    SEED,
    tx("s", 1, delete("c", {"_id": 1}, 1), start=True),
    ("b", None, tx("t", 1, delete("c", {"_id": 1}, 1), start=True)),
    commit("s", 1),
    ALL,
)
scenario(
    "findAndModify against another transaction's document",
    SEED,
    tx("s", 1, update("c", {"_id": 1}, SET9), start=True),
    ("b", None, tx("t", 1, fam("c", {"_id": 1}, update={"$set": {"v": 8}}), start=True)),
    commit("s", 1),
    ALL,
)
scenario(
    "a transaction updates a document changed since it began",
    SEED,
    tx("s", 1, ALL, start=True),
    ("b", None, update("c", {"_id": 1}, {"$set": {"v": 8}})),
    tx("s", 1, update("c", {"_id": 1}, SET9)),
    tx("s", 1, ALL),
    commit("s", 1),
    ALL,
)
scenario(
    "a transaction deletes a document deleted since it began",
    SEED,
    tx("s", 1, ALL, start=True),
    ("b", None, delete("c", {"_id": 1}, 1)),
    tx("s", 1, delete("c", {"_id": 1}, 1)),
    commit("s", 1),
    ALL,
)
scenario(
    "a transaction inserts an _id inserted since it began",
    SEED,
    tx("s", 1, ALL, start=True),
    ("b", None, insert("c", {"_id": 3, "by": "b"})),
    tx("s", 1, insert("c", {"_id": 3, "by": "s"})),
    commit("s", 1),
    ALL,
)
scenario(
    "transactions on different documents do not conflict",
    SEED,
    tx("s", 1, update("c", {"_id": 1}, SET9), start=True),
    ("b", None, tx("t", 1, update("c", {"_id": 2}, {"$set": {"v": 8}}), start=True)),
    commit("s", 1),
    ("b", "admin", tx("t", 1, {"commitTransaction": 1})),
    ALL,
)
scenario(
    "an ordinary write waits for a transaction's document",
    SEED,
    tx("s", 1, update("c", {"_id": 1}, SET9), start=True),
    ("b", None, {**update("c", {"_id": 1}, {"$set": {"v": 8}}), "maxTimeMS": 300}),
    ("b", None, {**update("c", {"_id": 2}, {"$set": {"v": 8}}), "maxTimeMS": 300}),
    commit("s", 1),
    ALL,
)
scenario(
    "a unique index across two transactions",
    SEED,
    {
        "createIndexes": "c",
        "indexes": [{"key": {"k": 1}, "name": "k", "unique": True, "sparse": True}],
    },
    tx("s", 1, insert("c", {"_id": 3, "k": "x"}), start=True),
    ("b", None, tx("t", 1, insert("c", {"_id": 4, "k": "x"}), start=True)),
    commit("s", 1),
    ALL,
)
scenario(
    "a unique index against a committed value",
    SEED,
    {
        "createIndexes": "c",
        "indexes": [{"key": {"k": 1}, "name": "k", "unique": True, "sparse": True}],
    },
    insert("c", {"_id": 3, "k": "x"}),
    tx("s", 1, insert("c", {"_id": 4, "k": "x"}), start=True),
    tx("s", 1, ALL),
    commit("s", 1),
    ALL,
)

# --------------------------------------------------- errors inside a transaction

scenario(
    "a duplicate key aborts the transaction",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    tx("s", 1, insert("c", {"_id": 1})),
    tx("s", 1, ALL),
    commit("s", 1),
    ALL,
)
scenario(
    "a duplicate key on the first statement",
    SEED,
    tx("s", 1, insert("c", {"_id": 1}), start=True),
    tx("s", 1, ALL),
    commit("s", 1),
)
scenario(
    "a duplicate inside one insert",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}, {"_id": 3}, {"_id": 4}), start=True),
    commit("s", 1),
    ALL,
)
scenario(
    "an unordered insert with a duplicate",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}, {"_id": 1}, {"_id": 4}, ordered=False), start=True),
    commit("s", 1),
    ALL,
)
scenario(
    "a bad update operator aborts the transaction",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    tx("s", 1, update("c", {"_id": 1}, {"$bogus": {"v": 1}})),
    tx("s", 1, ALL),
    commit("s", 1),
    ALL,
)
scenario(
    "a failed read aborts the transaction",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    tx("s", 1, find("c", {"$bogus": 1})),
    tx("s", 1, ALL),
    commit("s", 1),
    ALL,
)
scenario(
    "a failed aggregate aborts the transaction",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    tx("s", 1, agg("c", {"$project": {"x": {"$divide": [1, 0]}}})),
    commit("s", 1),
    ALL,
)
scenario(
    "an unknown command inside a transaction",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    tx("s", 1, {"bogusCommand": 1}),
    tx("s", 1, ALL),
    commit("s", 1),
    ALL,
)
scenario(
    "a validator refusal inside a transaction",
    {"create": "c", "validator": {"v": {"$gte": 0}}},
    tx("s", 1, insert("c", {"_id": 1, "v": 1}), start=True),
    tx("s", 1, insert("c", {"_id": 2, "v": -1})),
    tx("s", 1, ALL),
    commit("s", 1),
    ALL,
)
scenario(
    "an update that matches nothing is not an error",
    SEED,
    tx("s", 1, update("c", {"_id": 99}, SET9), start=True),
    tx("s", 1, delete("c", {"_id": 99})),
    tx("s", 1, fam("c", {"_id": 99}, remove=True)),
    commit("s", 1),
)

# ------------------------------------------ commands a transaction refuses

REFUSED = [
    ("count", {"count": "c"}),
    ("listCollections", {"listCollections": 1}),
    ("listIndexes", {"listIndexes": "c"}),
    ("drop", {"drop": "c"}),
    ("dropDatabase", {"dropDatabase": 1}),
    ("dropIndexes", {"dropIndexes": "c", "index": "*"}),
    ("collMod", {"collMod": "c", "validator": {}}),
    ("explain", {"explain": find("c")}),
    ("collStats", {"collStats": "c"}),
    ("dbStats", {"dbStats": 1}),
    ("ping", {"ping": 1}),
    ("hello", {"hello": 1}),
    ("buildInfo", {"buildInfo": 1}),
    ("getMore of nothing", {"getMore": Int64(12345), "collection": "c"}),
    ("killCursors", {"killCursors": "c", "cursors": [Int64(12345)]}),
    (
        "createIndexes on a collection with documents",
        {"createIndexes": "c", "indexes": [{"key": {"v": 1}, "name": "v"}]},
    ),
    ("create of an existing collection", {"create": "c"}),
    ("validate", {"validate": "c"}),
    (
        "mapReduce",
        {"mapReduce": "c", "map": "function(){}", "reduce": "function(){}", "out": {"inline": 1}},
    ),
    ("$out", agg("c", {"$out": "o"})),
    ("$merge", agg("c", {"$merge": {"into": "o"}})),
    ("$changeStream", agg("c", {"$changeStream": {}})),
    ("$collStats", agg("c", {"$collStats": {"count": {}}})),
    ("$indexStats", agg("c", {"$indexStats": {}})),
    ("$unionWith", agg("c", {"$unionWith": "c"})),
    (
        "$graphLookup",
        agg(
            "c",
            {
                "$graphLookup": {
                    "from": "c",
                    "startWith": "$v",
                    "connectFromField": "v",
                    "connectToField": "_id",
                    "as": "g",
                }
            },
        ),
    ),
    ("$facet", agg("c", {"$facet": {"a": [{"$count": "n"}]}})),
    ("find with a tailable cursor", find("c", tailable=True)),
    ("find with $where", find("c", {"$where": "true"})),
    ("endSessions", {"endSessions": []}),
    ("startSession", {"startSession": 1}),
    ("refreshSessions", {"refreshSessions": []}),
    (
        "insert with bypassDocumentValidation",
        insert("c", {"_id": 9}, bypassDocumentValidation=True),
    ),
    ("isMaster", {"isMaster": 1}),
    ("serverStatus", {"serverStatus": 1}),
    ("whatsmyuri", {"whatsmyuri": 1}),
    ("connectionStatus", {"connectionStatus": 1}),
    ("profile", {"profile": -1}),
    ("hostInfo", {"hostInfo": 1}),
    ("convertToCapped", {"convertToCapped": "c", "size": 4096}),
    ("killSessions", {"killSessions": []}),
    ("a find that fails to parse", {"find": "c", "filter": 1}),
    ("an update with no statements", {"update": "c", "updates": []}),
    (
        "$lookup with a pipeline",
        agg("c", {"$lookup": {"from": "c", "pipeline": [{"$limit": 1}], "as": "j"}}),
    ),
    ("$sample", agg("c", {"$sample": {"size": 1}}, {"$project": {"_id": 0, "x": {"$literal": 1}}})),
    ("$documents", {"aggregate": 1, "pipeline": [{"$documents": [{"a": 1}]}], "cursor": {}}),
    ("$listSessions", {"aggregate": 1, "pipeline": [{"$listLocalSessions": {}}], "cursor": {}}),
    ("$geoNear without an index", agg("c", {"$geoNear": {"near": [0, 0], "distanceField": "d"}})),
    (
        "$setWindowFields",
        agg("c", {"$setWindowFields": {"sortBy": {"_id": 1}, "output": {"r": {"$rank": {}}}}}),
    ),
    (
        "$group and $sort",
        agg("c", {"$group": {"_id": "$v", "n": {"$sum": 1}}}, {"$sort": {"_id": 1}}),
    ),
    ("distinct with a query", {"distinct": "c", "key": "v", "query": {"v": {"$gte": 2}}}),
    ("an expression that fails", agg("c", {"$project": {"x": {"$add": ["$v", "a"]}}})),
    ("find with maxTimeMS", find("c", maxTimeMS=1000)),
    ("find with a collation", find("c", collation={"locale": "en"})),
    ("find with allowDiskUse", find("c", sort={"v": 1}, allowDiskUse=True)),
    ("findAndModify with a bad argument", {"findAndModify": "c"}),
]
for label, command in REFUSED:
    scenario(
        f"in a transaction: {label} first",
        SEED,
        tx("s", 1, command, start=True),
        tx("s", 1, ALL),
        commit("s", 1),
    )
    scenario(
        f"in a transaction: {label} later",
        SEED,
        tx("s", 1, insert("c", {"_id": 3}), start=True),
        tx("s", 1, command),
        tx("s", 1, ALL),
        commit("s", 1),
        ALL,
    )
scenario(
    "in a transaction: renameCollection",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    ("a", "admin", tx("s", 1, {"renameCollection": "@db.c", "to": "@db.d"})),
    commit("s", 1),
    ALL,
)
scenario(
    "in a transaction: bulkWrite",
    SEED,
    (
        "a",
        "admin",
        tx(
            "s",
            1,
            {
                "bulkWrite": 1,
                "ops": [
                    {"insert": 0, "document": {"_id": 3}},
                    {"update": 0, "filter": {"_id": 1}, "updateMods": SET9},
                ],
                "nsInfo": [{"ns": "@db.c"}],
            },
            start=True,
        ),
    ),
    tx("s", 1, ALL),
    commit("s", 1),
    ALL,
)
scenario(
    "in a transaction: admin, config and local",
    SEED,
    ("a", "admin", tx("s", 1, insert("c", {"_id": 1}), start=True)),
    ("a", "config", tx("s", 2, insert("c", {"_id": 1}), start=True)),
    ("a", "local", tx("s", 3, insert("c", {"_id": 1}), start=True)),
    ("a", "local", tx("s", 4, find("c"), start=True)),
    ("a", "config", tx("s", 5, find("c"), start=True)),
    ("a", "admin", tx("s", 6, find("c"), start=True)),
    ("a", "admin", tx("s", 6, {"commitTransaction": 1})),
)
scenario(
    "in a transaction: system collections",
    SEED,
    tx("s", 1, insert("system.views", {"_id": "x"}), start=True),
    tx("s", 2, insert("system.js", {"_id": "x"}), start=True),
    tx("s", 3, find("system.views"), start=True),
)
scenario(
    "in a transaction: a capped collection",
    {"create": "cap", "capped": True, "size": 4096},
    insert("cap", {"_id": 1}),
    tx("s", 1, find("cap"), start=True),
    tx("s", 1, insert("cap", {"_id": 2})),
    commit("s", 1),
    tx("s", 2, update("cap", {"_id": 1}, SET9), start=True),
    tx("s", 3, delete("cap", {"_id": 1}), start=True),
    find("cap"),
)
scenario(
    "in a transaction: a view",
    SEED,
    {"create": "vw", "viewOn": "c", "pipeline": [{"$match": {"v": {"$gte": 2}}}]},
    tx("s", 1, find("vw"), start=True),
    tx("s", 1, agg("vw", {"$count": "n"})),
    tx("s", 1, insert("vw", {"_id": 9})),
    commit("s", 1),
)
scenario(
    "in a transaction: a time-series collection",
    {"create": "ts", "timeseries": {"timeField": "t"}},
    tx("s", 1, find("ts"), start=True),
    tx("s", 2, insert("ts", {"t": __import__("datetime").datetime(2026, 1, 1)}), start=True),
    commit("s", 2),
)

# -------------------------------------------- collections made in a transaction

scenario(
    "an insert creates the collection",
    tx("s", 1, insert("n", {"_id": 1}), start=True),
    tx("s", 1, find("n")),
    ("b", None, find("n")),
    ("b", None, {"listCollections": 1, "nameOnly": True}),
    commit("s", 1),
    find("n"),
    {"listCollections": 1, "nameOnly": True},
)
scenario(
    "an insert creates the collection, then abort",
    tx("s", 1, insert("n", {"_id": 1}), start=True),
    abort("s", 1),
    find("n"),
    {"listCollections": 1, "nameOnly": True},
)
scenario(
    "an upsert creates the collection",
    tx("s", 1, update("n", {"_id": 1}, SET9, upsert=True), start=True),
    commit("s", 1),
    find("n"),
)
scenario(
    "findAndModify with upsert creates the collection",
    tx("s", 1, fam("n", {"_id": 1}, update=SET9, upsert=True, new=True), start=True),
    commit("s", 1),
    find("n"),
)
scenario(
    "create inside a transaction",
    tx("s", 1, {"create": "n"}, start=True),
    tx("s", 1, insert("n", {"_id": 1})),
    ("b", None, {"listCollections": 1, "nameOnly": True}),
    commit("s", 1),
    find("n"),
    {"listCollections": 1, "nameOnly": True},
)
scenario(
    "create inside a transaction, then abort",
    tx("s", 1, {"create": "n"}, start=True),
    tx("s", 1, insert("n", {"_id": 1})),
    abort("s", 1),
    find("n"),
    {"listCollections": 1, "nameOnly": True},
)
scenario(
    "create with options inside a transaction",
    tx("s", 1, {"create": "n", "validator": {"v": {"$gte": 0}}}, start=True),
    tx("s", 1, insert("n", {"_id": 1, "v": -1})),
    commit("s", 1),
    tx("s", 2, {"create": "cap", "capped": True, "size": 4096}, start=True),
    tx("s", 3, {"create": "vw", "viewOn": "n", "pipeline": []}, start=True),
    tx("s", 4, {"create": "ts", "timeseries": {"timeField": "t"}}, start=True),
    {"listCollections": 1, "nameOnly": True},
)
scenario(
    "createIndexes inside a transaction",
    tx(
        "s",
        1,
        {"createIndexes": "n", "indexes": [{"key": {"v": 1}, "name": "v", "unique": True}]},
        start=True,
    ),
    tx("s", 1, insert("n", {"_id": 1, "v": 1})),
    tx("s", 1, insert("n", {"_id": 2, "v": 1})),
    commit("s", 1),
    {"listIndexes": "n"},
    find("n"),
)
scenario(
    "createIndexes on a collection the transaction made",
    tx("s", 1, {"create": "n"}, start=True),
    tx("s", 1, {"createIndexes": "n", "indexes": [{"key": {"v": 1}, "name": "v"}]}),
    tx("s", 1, insert("n", {"_id": 1, "v": 1})),
    commit("s", 1),
    {"listIndexes": "n"},
    find("n"),
)
scenario(
    "createIndexes after an insert in the same transaction",
    tx("s", 1, insert("n", {"_id": 1, "v": 1}), start=True),
    tx("s", 1, {"createIndexes": "n", "indexes": [{"key": {"v": 1}, "name": "v"}]}),
    commit("s", 1),
    find("n"),
)
scenario(
    "createIndexes on an empty existing collection",
    {"create": "n"},
    tx("s", 1, {"createIndexes": "n", "indexes": [{"key": {"v": 1}, "name": "v"}]}, start=True),
    commit("s", 1),
    {"listIndexes": "n"},
)
scenario(
    "createIndexes of an index that exists",
    SEED,
    {"createIndexes": "c", "indexes": [{"key": {"v": 1}, "name": "v"}]},
    tx("s", 1, {"createIndexes": "c", "indexes": [{"key": {"v": 1}, "name": "v"}]}, start=True),
    tx("s", 1, ALL),
    commit("s", 1),
)
scenario(
    "two transactions create one collection",
    tx("s", 1, insert("n", {"_id": 1}), start=True),
    ("b", None, tx("t", 1, insert("n", {"_id": 2}), start=True)),
    commit("s", 1),
    ("b", "admin", tx("t", 1, {"commitTransaction": 1})),
    find("n"),
)
scenario(
    "two transactions create one collection: the one that commits first wins",
    tx("s", 1, insert("n", {"_id": 1}), start=True),
    ("b", None, tx("t", 1, insert("n", {"_id": 2}), start=True)),
    ("b", "admin", tx("t", 1, {"commitTransaction": 1})),
    commit("s", 1),
    find("n"),
)
scenario(
    "two transactions create one collection by name",
    tx("s", 1, {"create": "n"}, start=True),
    ("b", None, tx("t", 1, {"create": "n"}, start=True)),
    commit("s", 1),
    ("b", "admin", tx("t", 1, {"commitTransaction": 1})),
    {"listCollections": 1, "filter": {"name": "n"}, "nameOnly": True},
)
scenario(
    "a transaction creates a collection, a plain insert creates it too",
    tx("s", 1, insert("n", {"_id": 1}), start=True),
    ("b", None, {"insert": "n", "documents": [{"_id": 2}], "maxTimeMS": 500}),
    commit("s", 1),
    find("n"),
)
scenario(
    "a transaction creates a collection, which nobody else sees until it commits",
    tx("s", 1, insert("n", {"_id": 1}), start=True),
    tx("s", 1, find("n")),
    ("b", None, find("n")),
    ("b", None, {"listCollections": 1, "filter": {"name": "n"}, "nameOnly": True}),
    commit("s", 1),
    ("b", None, find("n")),
)
scenario(
    "a transaction creates a collection and aborts",
    tx("s", 1, insert("n", {"_id": 1}), start=True),
    abort("s", 1),
    {"listCollections": 1, "filter": {"name": "n"}, "nameOnly": True},
    insert("n", {"_id": 2}),
    find("n"),
)
scenario(
    "two transactions create one collection, one aborts",
    tx("s", 1, insert("n", {"_id": 1}), start=True),
    ("b", None, tx("t", 1, insert("n", {"_id": 2}), start=True)),
    abort("s", 1),
    ("b", "admin", tx("t", 1, {"commitTransaction": 1})),
    find("n"),
)
scenario(
    "two transactions create one collection, with the same _id",
    tx("s", 1, insert("n", {"_id": 1}), start=True),
    ("b", None, tx("t", 1, insert("n", {"_id": 1}), start=True)),
    commit("s", 1),
    ("b", "admin", tx("t", 1, {"commitTransaction": 1})),
    find("n"),
)
scenario(
    "a transaction creates a collection that another commits meanwhile",
    tx("s", 1, insert("n", {"_id": 1}), start=True),
    ("b", None, tx("t", 1, insert("n", {"_id": 2}), start=True)),
    commit("s", 1),
    ("b", None, tx("t", 1, insert("n", {"_id": 3}))),
    ("b", None, tx("t", 1, find("n"))),
    ("b", "admin", tx("t", 1, {"commitTransaction": 1})),
    find("n"),
)
scenario(
    "a collection dropped under a transaction",
    SEED,
    tx("s", 1, ALL, start=True),
    ("b", None, {"drop": "c", "maxTimeMS": 300}),
    tx("s", 1, ALL),
    commit("s", 1),
    ALL,
)
scenario(
    "an index built under a transaction",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    (
        "b",
        None,
        {"createIndexes": "c", "indexes": [{"key": {"v": 1}, "name": "v"}], "maxTimeMS": 300},
    ),
    commit("s", 1),
    ALL,
)

# -------------------------------------------------------------- cursors

scenario(
    "getMore inside a transaction",
    insert("c", *[{"_id": i} for i in range(1, 8)]),
    tx("s", 1, find("c", sort={"_id": 1}, batchSize=2), start=True, save="k"),
    ("b", None, insert("c", {"_id": 8})),
    tx("s", 1, {"getMore": Cur("k"), "collection": "c", "batchSize": 3}),
    tx("s", 1, {"getMore": Cur("k"), "collection": "c"}),
    commit("s", 1),
)
scenario(
    "a transaction's cursor after commit",
    insert("c", *[{"_id": i} for i in range(1, 8)]),
    tx("s", 1, find("c", sort={"_id": 1}, batchSize=2), start=True, save="k"),
    commit("s", 1),
    in_sess("s", {"getMore": Cur("k"), "collection": "c"}),
    {"getMore": Cur("k"), "collection": "c"},
)
scenario(
    "a transaction's cursor after abort",
    insert("c", *[{"_id": i} for i in range(1, 8)]),
    tx("s", 1, find("c", sort={"_id": 1}, batchSize=2), start=True, save="k"),
    abort("s", 1),
    in_sess("s", {"getMore": Cur("k"), "collection": "c"}),
)
scenario(
    "a transaction's cursor used outside it",
    insert("c", *[{"_id": i} for i in range(1, 8)]),
    tx("s", 1, find("c", sort={"_id": 1}, batchSize=2), start=True, save="k"),
    in_sess("s", {"getMore": Cur("k"), "collection": "c"}),
    {"getMore": Cur("k"), "collection": "c"},
    in_sess("t", {"getMore": Cur("k"), "collection": "c"}),
    tx("s", 1, {"getMore": Cur("k"), "collection": "c", "batchSize": 2}),
    commit("s", 1),
)
scenario(
    "an ordinary cursor used inside a transaction",
    insert("c", *[{"_id": i} for i in range(1, 8)]),
    in_sess("s", find("c", sort={"_id": 1}, batchSize=2), save="k"),
    tx("s", 1, {"getMore": Cur("k"), "collection": "c", "batchSize": 2}, start=True),
    tx("s", 2, ALL, start=True),
    tx("s", 2, {"getMore": Cur("k"), "collection": "c", "batchSize": 2}),
    in_sess("s", {"getMore": Cur("k"), "collection": "c", "batchSize": 2}),
)
scenario(
    "a session's cursor used from another session",
    insert("c", *[{"_id": i} for i in range(1, 8)]),
    in_sess("s", find("c", sort={"_id": 1}, batchSize=2), save="k"),
    in_sess("t", {"getMore": Cur("k"), "collection": "c", "batchSize": 2}),
    {"getMore": Cur("k"), "collection": "c", "batchSize": 2},
    in_sess("s", {"getMore": Cur("k"), "collection": "c", "batchSize": 2}),
    in_sess("t", {"killCursors": "c", "cursors": [Cur("k")]}),
    in_sess("s", {"killCursors": "c", "cursors": [Cur("k")]}),
)
scenario(
    "a cursor opened without a session, used with one",
    insert("c", *[{"_id": i} for i in range(1, 8)]),
    {**find("c", sort={"_id": 1}, batchSize=2), "save": "k"},
    in_sess("s", {"getMore": Cur("k"), "collection": "c", "batchSize": 2}),
    {"getMore": Cur("k"), "collection": "c", "batchSize": 2},
)
scenario(
    "killCursors on a transaction's cursor",
    insert("c", *[{"_id": i} for i in range(1, 8)]),
    tx("s", 1, find("c", sort={"_id": 1}, batchSize=2), start=True, save="k"),
    tx("s", 1, {"killCursors": "c", "cursors": [Cur("k")]}),
    tx("s", 1, ALL),
    commit("s", 1),
)
scenario(
    "an aggregate cursor inside a transaction",
    insert("c", *[{"_id": i} for i in range(1, 8)]),
    tx(
        "s",
        1,
        {"aggregate": "c", "pipeline": [{"$sort": {"_id": 1}}], "cursor": {"batchSize": 2}},
        start=True,
        save="k",
    ),
    tx("s", 1, insert("c", {"_id": 0})),
    tx("s", 1, {"getMore": Cur("k"), "collection": "c"}),
    commit("s", 1),
)

# ------------------------------------------------------- session commands

scenario(
    "startSession and endSessions",
    {"startSession": 1},
    {"startSession": 1, "bogus": 1},
    {"endSessions": [lsid("s")]},
    {"endSessions": []},
    {"endSessions": 1},
    {"endSessions": [{"id": 1}]},
    {"endSessions": [lsid("s"), lsid("t")], "bogus": 1},
    in_sess("s", {"endSessions": [lsid("s")]}),
)
scenario(
    "refreshSessions",
    {"refreshSessions": [lsid("s")]},
    {"refreshSessions": []},
    {"refreshSessions": 1},
    {"refreshSessions": [{"id": 1}]},
    {"refreshSessions": [lsid("s")], "bogus": 1},
)
scenario(
    "killSessions and killAllSessions",
    {"killSessions": [lsid("s")]},
    {"killSessions": []},
    {"killSessions": 1},
    {"killSessions": [{"id": 1}]},
    {"killAllSessions": []},
    {"killAllSessions": 1},
    {"killAllSessions": [{"user": "u", "db": "admin"}]},
    {"killAllSessionsByPattern": []},
    {"killAllSessionsByPattern": [{"lsid": lsid("s")}]},
)
scenario(
    "endSessions leaves an open transaction alone",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    {"endSessions": [lsid("s")]},
    tx("s", 1, ALL),
    commit("s", 1),
    ALL,
)
scenario(
    "killSessions aborts an open transaction",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    ("b", None, {"killSessions": [lsid("s")]}),
    tx("s", 1, ALL),
    commit("s", 1),
    ALL,
    tx("s", 2, insert("c", {"_id": 4}), start=True),
    commit("s", 2),
    ALL,
)
scenario(
    "killSessions kills the session's cursors",
    insert("c", *[{"_id": i} for i in range(1, 8)]),
    in_sess("s", find("c", sort={"_id": 1}, batchSize=2), save="k"),
    {"killSessions": [lsid("s")]},
    in_sess("s", {"getMore": Cur("k"), "collection": "c"}),
)
scenario(
    "endSessions kills the session's cursors",
    insert("c", *[{"_id": i} for i in range(1, 8)]),
    in_sess("s", find("c", sort={"_id": 1}, batchSize=2), save="k"),
    {"endSessions": [lsid("s")]},
    in_sess("s", {"getMore": Cur("k"), "collection": "c"}),
)
scenario(
    "an ordinary command in a session with an open transaction",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    in_sess("s", ALL),
    in_sess("s", insert("c", {"_id": 4})),
    tx("s", 1, ALL),
    commit("s", 1),
    ALL,
)
scenario(
    "a retryable write in a session with an open transaction",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    retry("s", 1, insert("c", {"_id": 4})),
    retry("s", 2, insert("c", {"_id": 5})),
    tx("s", 1, ALL),
    commit("s", 1),
    ALL,
)

# --------------------------------------------------------- retryable writes

scenario(
    "a retried insert is applied once",
    SEED,
    retry("s", 1, insert("c", {"_id": 3})),
    retry("s", 1, insert("c", {"_id": 3})),
    ALL,
)
scenario(
    "a retried update is applied once",
    SEED,
    retry("s", 1, update("c", {"_id": 1}, {"$inc": {"v": 1}})),
    retry("s", 1, update("c", {"_id": 1}, {"$inc": {"v": 1}})),
    retry("s", 2, update("c", {"_id": 1}, {"$inc": {"v": 1}})),
    ALL,
)
scenario(
    "a retried upsert reports the same upserted id",
    SEED,
    retry("s", 1, update("c", {"_id": 5}, {"$set": {"v": 5}}, upsert=True)),
    retry("s", 1, update("c", {"_id": 5}, {"$set": {"v": 5}}, upsert=True)),
    ALL,
)
scenario(
    "a retried delete is applied once",
    SEED,
    insert("c", {"_id": 3, "v": 1}),
    retry("s", 1, delete("c", {"v": 1}, 1)),
    retry("s", 1, delete("c", {"v": 1}, 1)),
    ALL,
)
scenario(
    "a retried findAndModify returns the first answer",
    SEED,
    retry("s", 1, fam("c", {"_id": 1}, update={"$inc": {"v": 1}}, new=True)),
    retry("s", 1, fam("c", {"_id": 1}, update={"$inc": {"v": 1}}, new=True)),
    retry("s", 2, fam("c", {"_id": 1}, update={"$inc": {"v": 1}})),
    retry("s", 2, fam("c", {"_id": 1}, update={"$inc": {"v": 1}})),
    retry("s", 3, fam("c", {"_id": 2}, remove=True)),
    retry("s", 3, fam("c", {"_id": 2}, remove=True)),
    retry("s", 4, fam("c", {"_id": 9}, update={"$set": {"v": 9}}, upsert=True, new=True)),
    retry("s", 4, fam("c", {"_id": 9}, update={"$set": {"v": 9}}, upsert=True, new=True)),
    ALL,
)
scenario(
    "a retried batch reports every statement",
    SEED,
    retry("s", 1, insert("c", {"_id": 3}, {"_id": 4}, {"_id": 5})),
    retry("s", 1, insert("c", {"_id": 3}, {"_id": 4}, {"_id": 5})),
    ALL,
)
scenario(
    "a retried batch that failed part-way",
    SEED,
    retry("s", 1, insert("c", {"_id": 3}, {"_id": 1}, {"_id": 5})),
    retry("s", 1, insert("c", {"_id": 3}, {"_id": 1}, {"_id": 5})),
    ALL,
)
scenario(
    "one transaction number, a different statement",
    SEED,
    retry("s", 1, insert("c", {"_id": 3})),
    retry("s", 1, insert("c", {"_id": 4})),
    retry("s", 1, update("c", {"_id": 1}, SET9)),
    retry("s", 1, insert("c", {"_id": 5}), stmtId=1),
    retry("s", 1, insert("c", {"_id": 6}), stmtId=1),
    ALL,
)
scenario(
    "statement ids on a retried batch",
    SEED,
    retry("s", 1, insert("c", {"_id": 3}, {"_id": 4}), stmtIds=[5, 6]),
    retry("s", 1, insert("c", {"_id": 9}, {"_id": 4}), stmtIds=[7, 6]),
    ALL,
)
scenario(
    "an older retryable transaction number",
    SEED,
    retry("s", 5, insert("c", {"_id": 3})),
    retry("s", 4, insert("c", {"_id": 4})),
    retry("s", 6, insert("c", {"_id": 5})),
    retry("s", 5, insert("c", {"_id": 3})),
    ALL,
)
scenario(
    "retryable writes that are not retryable",
    SEED,
    retry("s", 1, update("c", {}, SET9, multi=True)),
    retry("s", 2, delete("c", {})),
    retry("s", 3, insert("c", {"_id": 3}, writeConcern={"w": 0})),
    ALL,
)
scenario(
    "a transaction number on a command that is not a write",
    SEED,
    retry("s", 1, ALL),
    retry("s", 2, agg("c")),
    retry("s", 3, {"count": "c"}),
    retry("s", 4, {"ping": 1}),
    retry("s", 5, {"distinct": "c", "key": "v"}),
    retry("s", 6, {"listCollections": 1}),
    retry("s", 7, {"createIndexes": "c", "indexes": [{"key": {"v": 1}, "name": "v"}]}),
    retry("s", 8, {"drop": "c"}),
)
scenario(
    "a retryable write then a transaction with its number",
    SEED,
    retry("s", 1, insert("c", {"_id": 3})),
    tx("s", 1, insert("c", {"_id": 4}), start=True),
    tx("s", 1, insert("c", {"_id": 5})),
    tx("s", 2, insert("c", {"_id": 6}), start=True),
    commit("s", 2),
    retry("s", 2, insert("c", {"_id": 7})),
    ALL,
)
scenario(
    "a retryable write that hits a duplicate key",
    SEED,
    retry("s", 1, insert("c", {"_id": 1})),
    retry("s", 1, insert("c", {"_id": 1})),
    retry("s", 2, update("c", {"_id": 1}, {"$bogus": 1})),
    retry("s", 2, update("c", {"_id": 1}, {"$bogus": 1})),
)
scenario(
    "a retried write after the document changed",
    SEED,
    retry("s", 1, update("c", {"_id": 1}, {"$inc": {"v": 1}})),
    update("c", {"_id": 1}, {"$set": {"v": 100}}),
    retry("s", 1, update("c", {"_id": 1}, {"$inc": {"v": 1}})),
    retry("s", 2, fam("c", {"_id": 2}, update={"$inc": {"v": 1}}, new=True)),
    delete("c", {"_id": 2}),
    retry("s", 2, fam("c", {"_id": 2}, update={"$inc": {"v": 1}}, new=True)),
    ALL,
)

scenario(
    "a transaction number on other commands",
    SEED,
    retry("s", 1, {"hello": 1}),
    retry("s", 2, {"isMaster": 1}),
    retry("s", 3, {"buildInfo": 1}),
    retry("s", 4, {"serverStatus": 1}),
    retry("s", 5, {"endSessions": []}),
    retry("s", 6, {"getMore": Int64(1), "collection": "c"}),
    retry("s", 7, {"killCursors": "c", "cursors": [Int64(1)]}),
    retry("s", 8, {"create": "z"}),
    retry("s", 9, {"explain": find("c")}),
    retry("s", 10, {"bogusCommand": 1}),
    retry("s", 11, {"startSession": 1}),
    retry("s", 12, {"refreshSessions": []}),
    retry("s", 13, {"whatsmyuri": 1}),
    retry("s", 14, {"listIndexes": "c"}),
    retry("s", 15, {"dropDatabase": 1}),
)
scenario(
    "a retried statement that wrote nothing",
    SEED,
    retry("s", 1, update("c", {"_id": 99}, SET9)),
    insert("c", {"_id": 99, "v": 0}),
    retry("s", 1, update("c", {"_id": 99}, SET9)),
    retry("s", 2, update("c", {"_id": 1}, {"$set": {"v": 1}})),
    update("c", {"_id": 1}, {"$set": {"v": 5}}),
    retry("s", 2, update("c", {"_id": 1}, {"$set": {"v": 1}})),
    retry("s", 3, delete("c", {"_id": 77}, 1)),
    insert("c", {"_id": 77}),
    retry("s", 3, delete("c", {"_id": 77}, 1)),
    retry("s", 4, fam("c", {"_id": 55}, update=SET9)),
    insert("c", {"_id": 55}),
    retry("s", 4, fam("c", {"_id": 55}, update=SET9)),
    retry("s", 5, fam("c", {"_id": 2}, update={"$set": {"v": 2}}, new=True)),
    update("c", {"_id": 2}, {"$set": {"v": 6}}),
    retry("s", 5, fam("c", {"_id": 2}, update={"$set": {"v": 2}}, new=True)),
    ALL,
)
scenario(
    "a retry with a different kind of command",
    SEED,
    retry("s", 1, insert("c", {"_id": 3})),
    retry("s", 1, delete("c", {"_id": 1}, 1)),
    retry("s", 1, fam("c", {"_id": 1}, update=SET9)),
    retry("s", 1, fam("c", {"_id": 1}, remove=True)),
    retry("s", 2, update("c", {"_id": 1}, SET9)),
    retry("s", 2, insert("c", {"_id": 4})),
    retry("s", 2, delete("c", {"_id": 2}, 1)),
    retry("s", 2, fam("c", {"_id": 2}, update=SET9)),
    retry("s", 2, fam("c", {"_id": 2}, remove=True)),
    retry("s", 3, delete("c", {"_id": 2}, 1)),
    retry("s", 3, insert("c", {"_id": 5})),
    retry("s", 3, update("c", {"_id": 1}, {"$set": {"v": 7}})),
    retry("s", 3, fam("c", {"_id": 1}, update=SET9)),
    retry("s", 3, fam("c", {"_id": 1}, remove=True)),
    retry("s", 4, fam("c", {"_id": 1}, update={"$set": {"v": 4}})),
    retry("s", 4, insert("c", {"_id": 6})),
    retry("s", 4, update("c", {"_id": 1}, {"$set": {"v": 7}})),
    retry("s", 4, delete("c", {"_id": 1}, 1)),
    retry("s", 4, fam("c", {"_id": 1}, remove=True)),
    retry("s", 4, fam("c", {"_id": 1}, update={"$set": {"v": 4}}, new=True)),
    retry("s", 5, fam("c", {"_id": 1}, remove=True)),
    retry("s", 5, fam("c", {"_id": 1}, update=SET9)),
    ALL,
)
scenario(
    "a retried update batch",
    SEED,
    retry(
        "s",
        1,
        {
            "update": "c",
            "updates": [
                {"q": {"_id": 1}, "u": {"$inc": {"v": 1}}},
                {"q": {"_id": 9}, "u": {"$set": {"v": 9}}, "upsert": True},
                {"q": {"_id": 50}, "u": {"$set": {"v": 9}}},
                {"q": {"_id": 2}, "u": {"$set": {"v": 2}}},
            ],
        },
    ),
    retry(
        "s",
        1,
        {
            "update": "c",
            "updates": [
                {"q": {"_id": 1}, "u": {"$inc": {"v": 1}}},
                {"q": {"_id": 9}, "u": {"$set": {"v": 9}}, "upsert": True},
                {"q": {"_id": 50}, "u": {"$set": {"v": 9}}},
                {"q": {"_id": 2}, "u": {"$set": {"v": 2}}},
            ],
        },
    ),
    retry(
        "s",
        2,
        {
            "delete": "c",
            "deletes": [
                {"q": {"_id": 1}, "limit": 1},
                {"q": {"_id": 40}, "limit": 1},
                {"q": {"_id": 2}, "limit": 1},
            ],
        },
    ),
    retry(
        "s",
        2,
        {
            "delete": "c",
            "deletes": [
                {"q": {"_id": 1}, "limit": 1},
                {"q": {"_id": 40}, "limit": 1},
                {"q": {"_id": 2}, "limit": 1},
            ],
        },
    ),
    ALL,
)
scenario(
    "a retried unordered batch",
    SEED,
    retry("s", 1, insert("c", {"_id": 3}, {"_id": 1}, {"_id": 5}, ordered=False)),
    retry("s", 1, insert("c", {"_id": 3}, {"_id": 1}, {"_id": 5}, ordered=False)),
    ALL,
)
scenario(
    "statement ids of the wrong shape",
    SEED,
    retry("s", 1, insert("c", {"_id": 3}), stmtId="x"),
    retry("s", 2, insert("c", {"_id": 4}), stmtIds=[1, 2]),
    retry("s", 3, insert("c", {"_id": 5}), stmtIds=[1], stmtId=1),
    retry("s", 4, insert("c", {"_id": 6}), stmtId=-5),
    retry("s", 4, insert("c", {"_id": 7}), stmtId=-5),
    insert("c", {"_id": 8}, stmtId=1),
    in_sess("s", insert("c", {"_id": 9}), stmtId=1),
    ALL,
)
scenario(
    "a retryable write to a collection that does not exist",
    retry("s", 1, insert("n", {"_id": 1})),
    retry("s", 1, insert("n", {"_id": 1})),
    retry("s", 2, update("m", {"_id": 1}, SET9, upsert=True)),
    retry("s", 2, update("m", {"_id": 1}, SET9, upsert=True)),
    find("n"),
    find("m"),
)
scenario(
    "read concern outside a transaction",
    SEED,
    {**ALL, "readConcern": {"level": "bogus"}},
    {**ALL, "readConcern": {"level": 1}},
    {**ALL, "readConcern": {"level": "local", "bogus": 1}},
    {**ALL, "readConcern": "local"},
    {**ALL, "readConcern": {"level": "linearizable"}},
    {**ALL, "readConcern": {"level": "available"}},
    {**ALL, "readConcern": {"level": "majority"}},
    {**ALL, "readConcern": {}},
    {**insert("c", {"_id": 3}), "readConcern": {"level": "majority"}},
    {**insert("c", {"_id": 4}), "readConcern": {"level": "local"}},
    {**insert("c", {"_id": 5}), "readConcern": {}},
    {"ping": 1, "readConcern": {"level": "majority"}},
    {"listCollections": 1, "nameOnly": True, "readConcern": {"level": "majority"}},
)
scenario(
    "a transaction's cursor in other transactions",
    insert("c", *[{"_id": i} for i in range(1, 8)]),
    tx("s", 1, find("c", sort={"_id": 1}, batchSize=2), start=True, save="k"),
    commit("s", 1),
    tx("s", 1, {"getMore": Cur("k"), "collection": "c", "batchSize": 2}),
    tx("s", 2, ALL, start=True),
    tx("s", 2, {"getMore": Cur("k"), "collection": "c", "batchSize": 2}),
    tx("s", 2, ALL),
    in_sess("s", {"killCursors": "c", "cursors": [Cur("k")]}),
)
scenario(
    "a cursor error inside a transaction",
    insert("c", *[{"_id": i} for i in range(1, 8)]),
    in_sess("s", find("c", sort={"_id": 1}, batchSize=2), save="k"),
    tx("s", 1, ALL, start=True),
    tx("s", 1, {"getMore": Cur("k"), "collection": "c", "batchSize": 2}),
    tx("s", 1, ALL),
    commit("s", 1),
)
scenario(
    "writes with several statements in a transaction",
    SEED,
    tx(
        "s",
        1,
        {
            "update": "c",
            "updates": [
                {"q": {"_id": 1}, "u": SET9},
                {"q": {"_id": 2}, "u": {"$bogus": 1}},
                {"q": {"_id": 2}, "u": SET9},
            ],
            "ordered": False,
        },
        start=True,
    ),
    commit("s", 1),
    tx(
        "s",
        2,
        {
            "delete": "c",
            "deletes": [
                {"q": {"_id": 1}, "limit": 1},
                {"q": {"$bogus": 1}, "limit": 1},
                {"q": {"_id": 2}, "limit": 1},
            ],
            "ordered": False,
        },
        start=True,
    ),
    commit("s", 2),
    ALL,
)
scenario(
    "a multi update and delete in a transaction",
    SEED,
    tx("s", 1, update("c", {}, {"$inc": {"v": 1}}, multi=True), start=True),
    tx("s", 1, delete("c", {"v": 3})),
    tx("s", 1, ALL),
    commit("s", 1),
    ALL,
)

# ------------------------------------------------------------ change streams

WATCH = {"aggregate": "c", "pipeline": [{"$changeStream": {}}], "cursor": {}, "save": "w"}
MORE = ("b", None, {"getMore": Cur("w"), "collection": "c", "maxTimeMS": 400})
scenario(
    "a committed transaction in a change stream",
    SEED,
    ("b", None, WATCH),
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    tx("s", 1, update("c", {"_id": 1}, SET9)),
    tx("s", 1, delete("c", {"_id": 2}, 1)),
    MORE,
    commit("s", 1),
    MORE,
)
scenario(
    "an aborted transaction in a change stream",
    SEED,
    ("b", None, WATCH),
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    abort("s", 1),
    MORE,
)
scenario(
    "a retryable write in a change stream",
    SEED,
    ("b", None, WATCH),
    retry("s", 1, insert("c", {"_id": 3})),
    retry("s", 1, insert("c", {"_id": 3})),
    in_sess("s", insert("c", {"_id": 4})),
    MORE,
)
scenario(
    "one document written twice in a transaction",
    SEED,
    ("b", None, WATCH),
    tx("s", 1, insert("c", {"_id": 3, "v": 0}), start=True),
    tx("s", 1, update("c", {"_id": 3}, SET9)),
    tx("s", 1, update("c", {"_id": 3}, {"$set": {"v": 10}})),
    tx("s", 1, delete("c", {"_id": 3}, 1)),
    commit("s", 1),
    MORE,
)


# ---------------------------------------------------------------- running


#: mongod remembers a session's transaction numbers for half an hour, so a
#: second run must not reuse the first one's session ids.
SALT = uuid.uuid4().hex


def session_id(run, label):
    return Binary(uuid.uuid5(uuid.NAMESPACE_DNS, f"{SALT}/{run}/{label}").bytes, 4)


# ------------------------------------------- phase 1b: writes (2026-10-14)

BULK = {
    "bulkWrite": 1,
    "ops": [
        {"insert": 0, "document": {"_id": 10}},
        {"update": 0, "filter": {"_id": 1}, "updateMods": {"$inc": {"v": 1}}},
    ],
    "nsInfo": [{"ns": "@db.c"}],
}

scenario(
    "a retried bulkWrite is replayed, not run again",
    SEED,
    ("a", "admin", retry("s", 1, BULK)),
    ("a", "admin", retry("s", 1, BULK)),
    ALL,
)
scenario(
    "a retried bulkWrite on a new connection is replayed",
    SEED,
    ("a", "admin", retry("s", 1, BULK)),
    ("b", "admin", retry("s", 1, BULK)),
    ALL,
)
scenario(
    "a bulkWrite under a newer txnNumber runs",
    SEED,
    ("a", "admin", retry("s", 1, BULK)),
    ("a", "admin", retry("s", 2, {**BULK, "ops": BULK["ops"][1:]})),
    ALL,
)
scenario(
    "two transactions create one collection, the later one commits first",
    tx("s", 1, insert("n", {"_id": 1}), start=True),
    ("b", None, tx("t", 1, insert("n", {"_id": 2}), start=True)),
    commit("t", 1, conn="b"),
    commit("s", 1),
    find("n", sort={"_id": 1}),
)
scenario(
    "a collection dropped under a transaction that wrote to it",
    SEED,
    tx("s", 1, insert("c", {"_id": 3}), start=True),
    tx("s", 1, update("c", {"_id": 1}, SET9)),
    ("b", None, {"drop": "c", "maxTimeMS": 500}),
    commit("s", 1),
    ALL,
    {"count": "c"},
    ("b", None, {"listIndexes": "c"}),
)
scenario(
    "a collection dropped under a transaction that only read it",
    SEED,
    tx("s", 1, ALL, start=True),
    ("b", None, {"drop": "c", "maxTimeMS": 500}),
    commit("s", 1),
    ALL,
)


def resolve(value, run, db, cursors):
    if isinstance(value, Sess):
        return session_id(run, value.label)
    if isinstance(value, Cur):
        return Int64(cursors.get(value.label, 987654321))
    if isinstance(value, dict):
        return {k: resolve(v, run, db, cursors) for k, v in value.items()}
    if isinstance(value, list):
        return [resolve(v, run, db, cursors) for v in value]
    if isinstance(value, str) and "@db" in value:
        return value.replace("@db", db)
    return value


UUID_TEXT = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")


#: Errors whose text mongod fills with a dump of the request or of its own
#: oplog entry. The code is compared and the text is not.
DUMPS = (40606, 40607, 40608, 40612)


def clean(value, key=None):
    if isinstance(value, dict):
        if value.get("code") in DUMPS and "errmsg" in value:
            value = {**value, "errmsg": "<names the request and the oplog entry>"}
        out = {}
        for k, v in value.items():
            if k in NOISE:
                continue
            if k == "_id" and isinstance(v, dict) and "_data" in v:
                continue
            if k in ("clusterTime", "wallTime", "postBatchResumeToken", "collectionUUID"):
                continue
            if k == "atClusterTime":
                out[k] = "TIME"
                continue
            if k == "id" and key in ("cursor", "startSession") and not isinstance(v, dict):
                out[k] = "CURSOR" if v else 0
                continue
            if k == "id" and isinstance(v, (Binary, uuid.UUID)):
                out[k] = "UUID"
                continue
            if k == "timeoutMinutes":
                out[k] = v
                continue
            out[k] = clean(v, k)
        return out
    if isinstance(value, list):
        if key in ("cursorsKilled", "cursorsNotFound", "cursorsAlive", "cursorsUnknown"):
            return ["CURSOR" if isinstance(v, int) and v > 10**6 else v for v in value]
        return [clean(v, key) for v in value]
    if isinstance(value, str):
        text = UUID_TEXT.sub("UUID", value)
        text = re.sub(r"Timestamp\(\d+, \d+\)", "Timestamp(T)", text)
        text = re.sub(r"cursor (id )?\d{6,}", r"cursor \1N", text)
        # mongod goes on to print the statement ids and the whole command.
        text = re.sub(r"\. (Statement ids|Got \{|Write command): .*$", ".", text)
        text = re.sub(r"\. Got \{.*$", ".", text)
        # A conflict on an index names mongod's own table for it, or nothing.
        text = re.sub(
            r"Caused by :: (WiredTiger\S+ index: \S+ uri: \S+)? ::", "Caused by :: <index> ::", text
        )
        return text
    return value


def run_scenario(uri, index, steps):
    """Every reply of one scenario, cleaned."""
    run = f"run{index}"
    db = f"txp_{index}"
    conns: dict[str, Wire] = {}
    cursors: dict[str, int] = {}
    open_txns: dict[str, int] = {}
    replies = []
    for step in steps:
        if isinstance(step, tuple):
            conn, where, command = step
        else:
            conn, where, command = "a", None, step
        command = dict(command)
        save = command.pop("save", None)
        if where is None:
            where = db
        elif where == "@other":
            where = db + "_o"
        session = command.get("lsid")
        if (
            isinstance(session, dict)
            and isinstance(session.get("id"), Sess)
            and isinstance(command.get("txnNumber"), int)
        ):
            open_txns[session["id"].label] = command["txnNumber"]
        wire = conns.get(conn)
        if wire is None:
            wire = conns[conn] = Wire(uri)
        try:
            reply = wire.command(where, resolve(command, run, db, cursors))
        except TimeoutError:
            wire.close()
            conns.pop(conn, None)
            replies.append({"probe": "TIMEOUT"})
            continue
        except (ConnectionError, OSError) as exc:
            wire.close()
            conns.pop(conn, None)
            replies.append({"probe": f"CONNECTION {type(exc).__name__}"})
            continue
        if save and isinstance(reply.get("cursor"), dict):
            cursors[save] = reply["cursor"].get("id", 0)
        replies.append(clean(reply))
    # Leave nothing open: a transaction left behind would block the drop.
    tidy = Wire(uri)
    try:
        for label, number in open_txns.items():
            tidy.command(
                "admin",
                {
                    "abortTransaction": 1,
                    "lsid": {"id": session_id(run, label)},
                    "txnNumber": Int64(number),
                    "autocommit": False,
                },
            )
        for name in (db, db + "_o"):
            tidy.command(name, {"dropDatabase": 1})
    except (TimeoutError, ConnectionError, OSError):
        print(f"  NOTE: cleanup of {db} did not finish on {uri}", file=sys.stderr)
    finally:
        tidy.close()
        for wire in conns.values():
            wire.close()
    return replies


def show(command):
    if isinstance(command, tuple):
        conn, where, command = command
        prefix = f"[{conn}{'' if where is None else ' ' + where}] "
    else:
        prefix = ""
    body = {k: v for k, v in command.items() if k not in ("lsid", "save")}
    sess = command.get("lsid", {}).get("id") if isinstance(command.get("lsid"), dict) else None
    tag = f"<{sess.label}> " if isinstance(sess, Sess) else ""
    return (prefix + tag + repr(body))[:230]


def main():
    mongod_uri = os.environ.get("PROBE_MONGOD", DEFAULT_MONGOD)
    server_uri = os.environ.get("PROBE_SERVER")
    if not server_uri:
        print("set PROBE_SERVER to the server under test", file=sys.stderr)
        return 2
    only = os.environ.get("PROBE_ONLY")
    verbose = os.environ.get("PROBE_VERBOSE")

    check = run_scenario(mongod_uri, 0, S[0][1])
    if not (check[2].get("ok") == 1 and len(check[3]["cursor"]["firstBatch"]) == 3):
        print("self-check failed on mongod: is it a replica-set member?", check, file=sys.stderr)
        return 2

    divergent = 0
    steps_total = steps_divergent = 0
    for index, (name, steps) in enumerate(S, start=1):
        if only and only not in name:
            continue
        want = run_scenario(mongod_uri, index, steps)
        got = run_scenario(server_uri, index, steps)
        steps_total += len(steps)
        bad = [i for i, (w, g) in enumerate(zip(want, got, strict=True)) if repr(w) != repr(g)]
        if not bad:
            continue
        divergent += 1
        steps_divergent += len(bad)
        print(f"\n--- {name}")
        for i in bad:
            print(f"  step {i}: {show(steps[i])}")
            print(f"    mongod: {str(want[i])[:900]}")
            print(f"    server: {str(got[i])[:900]}")
        if verbose:
            for i, step in enumerate(steps):
                print(f"   ({i}) {show(step)} -> {str(want[i])[:300]}")
    total = sum(1 for name, _ in S if not only or only in name)
    print(
        f"\n=== transactions: {total} scenarios, {steps_total} replies -- "
        f"{divergent} scenarios and {steps_divergent} replies differ ==="
    )
    return 1 if divergent else 0


if __name__ == "__main__":
    sys.exit(main())
