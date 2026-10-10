"""Differential-probe the `bulkWrite` COMMAND against a real mongod.

This is the 8.0 server command (`{bulkWrite: 1, ops, nsInfo}` on `admin`), not
a driver's `bulk_write` helper. Every scenario starts from the same seeded
collections, runs one command, drains its result cursor, and then reads the
collections back, so a difference in what was WRITTEN shows up beside a
difference in what was answered.

    PROBE_MONGOD=mongodb://127.0.0.1:27017 python tools/probes/bulk_write_command.py
"""

import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from _servers import probe_targets, report  # noqa: E402
from bson import Int64, ObjectId  # noqa: E402
from pymongo.errors import PyMongoError  # noqa: E402

NOISE = ("$clusterTime", "operationTime", "opTime", "electionId")
DB = "bw_probe"
C, D = f"{DB}.c", f"{DB}.d"
NS = [{"ns": C}, {"ns": D}]


def bw(*ops, ns=None, **extra):
    return {"bulkWrite": 1, "ops": list(ops), "nsInfo": NS if ns is None else ns, **extra}


def ins(doc, n=0):
    return {"insert": n, "document": doc}


def upd(filt, mods, n=0, **extra):
    return {"update": n, "filter": filt, "updateMods": mods, **extra}


def dele(filt, n=0, **extra):
    return {"delete": n, "filter": filt, **extra}


#: (label, command[, database])
#:
#: `ops` and `nsInfo` that are not arrays of documents are left out: pymongo
#: sends both as OP_MSG document sequences and fails on the client, identically
#: for every server, which a differential reads as agreement.
SCENARIOS = [
    # --- the plain shapes
    ("insert one", bw(ins({"_id": 10, "a": 1}))),
    ("insert no _id", bw(ins({"a": 1}))),
    ("insert two namespaces", bw(ins({"_id": 10}), ins({"_id": 10}, 1))),
    ("insert into a new collection", bw(ins({"_id": 1}), ns=[{"ns": f"{DB}.fresh"}])),
    ("update one", bw(upd({"_id": 1}, {"$set": {"a": 99}}))),
    ("update no match", bw(upd({"_id": 77}, {"$set": {"a": 99}}))),
    ("update no change", bw(upd({"_id": 1}, {"$set": {"a": 1}}))),
    ("update multi", bw(upd({}, {"$inc": {"a": 1}}, multi=True))),
    ("update without multi hits one", bw(upd({}, {"$inc": {"a": 1}}))),
    ("update replacement", bw(upd({"_id": 1}, {"z": 1}))),
    ("update replacement multi", bw(upd({}, {"z": 1}, multi=True))),
    ("update pipeline", bw(upd({"_id": 1}, [{"$set": {"b": {"$add": ["$a", 5]}}}]))),
    ("update empty mods", bw(upd({"_id": 1}, {}))),
    ("update bad operator", bw(upd({"_id": 1}, {"$nope": {"a": 1}}))),
    ("update changes _id", bw(upd({"_id": 1}, {"$set": {"_id": 50}}))),
    ("update upsert with _id", bw(upd({"_id": 40}, {"$set": {"a": 4}}, upsert=True))),
    ("update upsert no _id", bw(upd({"k": 40}, {"$set": {"a": 4}}, upsert=True))),
    ("update upsert matched", bw(upd({"_id": 1}, {"$set": {"a": 4}}, upsert=True))),
    ("update upsert multi", bw(upd({"k": 9}, {"$set": {"a": 4}}, upsert=True, multi=True))),
    (
        "update arrayFilters",
        bw(upd({"_id": 2}, {"$set": {"arr.$[e]": 0}}, arrayFilters=[{"e": {"$gt": 2}}])),
    ),
    ("update sort", bw(upd({}, {"$set": {"hit": True}}, sort={"a": -1}))),
    ("update sort with multi", bw(upd({}, {"$set": {"hit": True}}, sort={"a": -1}, multi=True))),
    ("update hint name", bw(upd({"a": 1}, {"$set": {"hit": True}}, hint="a_1"))),
    ("update hint missing", bw(upd({"a": 1}, {"$set": {"hit": True}}, hint="nope_1"))),
    (
        "update collation",
        bw(upd({"s": "B"}, {"$set": {"hit": True}}, collation={"locale": "en", "strength": 2})),
    ),
    ("update constants", bw(upd({"_id": 1}, [{"$set": {"b": "$$k"}}], constants={"k": 7}))),
    (
        "update with let",
        bw(upd({"$expr": {"$eq": ["$_id", "$$v"]}}, {"$set": {"hit": 1}}), let={"v": 2}),
    ),
    ("delete one", bw(dele({"_id": 1}))),
    ("delete no match", bw(dele({"_id": 77}))),
    ("delete without multi hits one", bw(dele({}))),
    ("delete multi", bw(dele({}, multi=True))),
    ("delete hint", bw(dele({"a": 1}, hint={"a": 1}))),
    ("delete hint missing", bw(dele({"a": 1}, hint="nope_1"))),
    ("delete collation", bw(dele({"s": "B"}, collation={"locale": "en", "strength": 2}))),
    (
        "mixed batch",
        bw(
            ins({"_id": 10}),
            upd({"_id": 10}, {"$set": {"a": 1}}),
            dele({"_id": 0}),
            ins({"_id": 11}, 1),
        ),
    ),
    # --- errors inside the batch
    ("dup key ordered", bw(ins({"_id": 1}), ins({"_id": 20}))),
    ("dup key unordered", bw(ins({"_id": 1}), ins({"_id": 20}), ordered=False)),
    ("dup key mid ordered", bw(ins({"_id": 20}), ins({"_id": 1}), ins({"_id": 21}))),
    (
        "dup key mid unordered",
        bw(ins({"_id": 20}), ins({"_id": 1}), ins({"_id": 21}), ordered=False),
    ),
    ("errorsOnly clean", bw(ins({"_id": 20}), ins({"_id": 21}), errorsOnly=True)),
    (
        "errorsOnly with error",
        bw(ins({"_id": 20}), ins({"_id": 1}), ins({"_id": 21}), errorsOnly=True, ordered=False),
    ),
    ("unique index violation", bw(ins({"_id": 20, "u": 1}), ins({"_id": 21, "u": 1}))),
    ("upsert dup key", bw(upd({"u": 5}, {"$set": {"_id": 1}}, upsert=True))),
    ("insert _id array", bw(ins({"_id": [1, 2]}))),
    ("insert $ field", bw(ins({"_id": 20, "$bad": 1}))),
    ("insert dotted field", bw(ins({"_id": 20, "a.b": 1}))),
    ("validator rejects", bw(ins({"_id": 20, "v": "no"}, 1))),
    ("validator rejects update", bw(upd({"_id": 0}, {"$set": {"v": "no"}}, 1))),
    (
        "validator bypassed",
        bw(ins({"_id": 20, "v": "no"}, 1), bypassDocumentValidation=True),
    ),
    ("write into a view", bw(ins({"_id": 20}), ns=[{"ns": f"{DB}.vw"}])),
    ("update a view", bw(upd({}, {"$set": {"a": 1}}), ns=[{"ns": f"{DB}.vw"}])),
    ("delete from a view", bw(dele({}), ns=[{"ns": f"{DB}.vw"}])),
    ("delete from capped", bw(dele({}), ns=[{"ns": f"{DB}.cap"}])),
    ("insert into capped", bw(ins({"_id": 20}), ns=[{"ns": f"{DB}.cap"}])),
    ("insert into system.views", bw(ins({"_id": "x"}), ns=[{"ns": f"{DB}.system.views"}])),
    ("insert into system.other", bw(ins({"_id": "x"}), ns=[{"ns": f"{DB}.system.other"}])),
    ("insert into local", bw(ins({"_id": "x"}), ns=[{"ns": "local.bwprobe"}])),
    ("insert into config db", bw(ins({"_id": "x"}), ns=[{"ns": "config.bwprobe"}])),
    # --- the cursor
    ("batchSize 2 of 5", bw(*[ins({"_id": 20 + i}) for i in range(5)], cursor={"batchSize": 2})),
    ("batchSize 2 of 2", bw(*[ins({"_id": 20 + i}) for i in range(2)], cursor={"batchSize": 2})),
    ("batchSize 0", bw(*[ins({"_id": 20 + i}) for i in range(3)], cursor={"batchSize": 0})),
    ("batchSize negative", bw(ins({"_id": 20}), cursor={"batchSize": -1})),
    ("batchSize string", bw(ins({"_id": 20}), cursor={"batchSize": "x"})),
    ("cursor not object", bw(ins({"_id": 20}), cursor=5)),
    ("cursor unknown field", bw(ins({"_id": 20}), cursor={"nope": 1})),
    # --- the command's own arguments
    ("not admin", bw(ins({"_id": 20})), DB),
    ("unknown field", bw(ins({"_id": 20}), bogus=1)),
    ("ops empty", bw()),
    ("ops missing", {"bulkWrite": 1, "nsInfo": NS}),
    ("nsInfo missing", {"bulkWrite": 1, "ops": [ins({"_id": 20})]}),
    ("nsInfo empty", bw(ins({"_id": 20}), ns=[])),
    ("nsInfo no ns", bw(ins({"_id": 20}), ns=[{}])),
    ("nsInfo ns not string", bw(ins({"_id": 20}), ns=[{"ns": 5}])),
    ("nsInfo ns no dot", bw(ins({"_id": 20}), ns=[{"ns": "nodot"}])),
    ("nsInfo ns empty db", bw(ins({"_id": 20}), ns=[{"ns": ".c"}])),
    ("nsInfo ns empty coll", bw(ins({"_id": 20}), ns=[{"ns": f"{DB}."}])),
    ("nsInfo ns $ coll", bw(ins({"_id": 20}), ns=[{"ns": f"{DB}.a$b"}])),
    ("nsInfo unknown field", bw(ins({"_id": 20}), ns=[{"ns": C, "bogus": 1}])),
    ("nsInfo duplicate ns", bw(ins({"_id": 20}), ins({"_id": 21}, 1), ns=[{"ns": C}, {"ns": C}])),
    ("index out of range", bw(ins({"_id": 20}, 5))),
    ("index negative", bw(ins({"_id": 20}, -1))),
    ("index string", bw({"insert": "c", "document": {"_id": 20}})),
    ("index double", bw({"insert": 0.0, "document": {"_id": 20}})),
    ("index long", bw({"insert": Int64(0), "document": {"_id": 20}})),
    ("op no kind", bw({"document": {"_id": 20}})),
    ("op unknown kind", bw({"replace": 0, "document": {"_id": 20}})),
    ("op two kinds", bw({"insert": 0, "delete": 0, "document": {"_id": 20}, "filter": {}})),
    ("op kind not first", bw({"document": {"_id": 20}, "insert": 0})),
    ("insert no document", bw({"insert": 0})),
    ("insert document not object", bw({"insert": 0, "document": 5})),
    ("insert unknown field", bw({"insert": 0, "document": {"_id": 20}, "bogus": 1})),
    ("update no updateMods", bw({"update": 0, "filter": {}})),
    ("update no filter", bw({"update": 0, "updateMods": {"$set": {"a": 1}}})),
    ("update filter not object", bw({"update": 0, "filter": 5, "updateMods": {"$set": {"a": 1}}})),
    ("update mods not object", bw({"update": 0, "filter": {}, "updateMods": 5})),
    ("update multi not bool", bw(upd({}, {"$set": {"a": 1}}, multi="yes"))),
    ("update upsert not bool", bw(upd({}, {"$set": {"a": 1}}, upsert=1))),
    ("update unknown field", bw(upd({}, {"$set": {"a": 1}}, bogus=1))),
    ("delete no filter", bw({"delete": 0})),
    ("delete filter not object", bw({"delete": 0, "filter": 5})),
    ("delete multi not bool", bw(dele({}, multi=1))),
    ("delete unknown field", bw(dele({}, bogus=1))),
    ("delete with sort", bw(dele({}, sort={"a": 1}))),
    ("ordered not bool", bw(ins({"_id": 20}), ordered=1)),
    ("errorsOnly not bool", bw(ins({"_id": 20}), errorsOnly="x")),
    ("bypass not bool", bw(ins({"_id": 20}), bypassDocumentValidation="x")),
    ("let not object", bw(ins({"_id": 20}), let=5)),
    ("comment", bw(ins({"_id": 20}), comment="hello")),
    ("writeConcern w1", bw(ins({"_id": 20}), writeConcern={"w": 1})),
    ("writeConcern w0", bw(ins({"_id": 20}), writeConcern={"w": 0})),
    ("writeConcern w5", bw(ins({"_id": 20}), writeConcern={"w": 5, "wtimeout": 100})),
    ("maxTimeMS", bw(ins({"_id": 20}), maxTimeMS=1000)),
    # --- which check comes first, and what was written before it
    ("ops and nsInfo missing", {"bulkWrite": 1}),
    ("unknown field and ops empty", bw(bogus=1)),
    ("good then unparsable op", bw(ins({"_id": 20}), {"insert": 0})),
    ("good then unknown op field", bw(ins({"_id": 20}), dele({}, bogus=1))),
    ("good then index out of range", bw(ins({"_id": 20}), ins({"_id": 21}, 5))),
    (
        "good then system.views",
        bw(ins({"_id": 20}), ins({"_id": "x"}, 1), ns=[{"ns": C}, {"ns": f"{DB}.system.views"}]),
    ),
    (
        "good then system.views unordered",
        bw(
            ins({"_id": 20}),
            ins({"_id": "x"}, 1),
            ins({"_id": 21}),
            ns=[{"ns": C}, {"ns": f"{DB}.system.views"}],
            ordered=False,
        ),
    ),
    (
        "good then $ collection",
        bw(ins({"_id": 20}), ins({"_id": "x"}, 1), ns=[{"ns": C}, {"ns": f"{DB}.a$b"}]),
    ),
    (
        "good then a view",
        bw(
            ins({"_id": 20}),
            ins({"_id": 21}, 1),
            ins({"_id": 22}),
            ns=[{"ns": C}, {"ns": f"{DB}.vw"}],
        ),
    ),
    (
        "good then a view unordered",
        bw(
            ins({"_id": 20}),
            ins({"_id": 21}, 1),
            ins({"_id": 22}),
            ns=[{"ns": C}, {"ns": f"{DB}.vw"}],
            ordered=False,
        ),
    ),
    ("unreferenced bad ns", bw(ins({"_id": 20}), ns=[{"ns": C}, {"ns": "nodot"}])),
    (
        "unreferenced system.views",
        bw(ins({"_id": 20}), ns=[{"ns": C}, {"ns": f"{DB}.system.views"}]),
    ),
    ("unreferenced view", bw(ins({"_id": 20}), ns=[{"ns": C}, {"ns": f"{DB}.vw"}])),
    ("update hint number", bw(upd({"a": 1}, {"$set": {"hit": True}}, hint=5))),
    ("update collation not object", bw(upd({"a": 1}, {"$set": {"hit": True}}, collation=5))),
    ("update arrayFilters not array", bw(upd({"a": 1}, {"$set": {"hit": True}}, arrayFilters=5))),
    ("update constants not object", bw(upd({"a": 1}, {"$set": {"hit": True}}, constants=5))),
    (
        "update constants on modifier update",
        bw(upd({"a": 1}, {"$set": {"hit": True}}, constants={"k": 1})),
    ),
    ("update sort not object", bw(upd({"a": 1}, {"$set": {"hit": True}}, sort=5))),
    (
        "update upsertSupplied",
        bw(
            upd(
                {"_id": 60},
                [{"$set": {"a": 1}}],
                upsert=True,
                upsertSupplied=True,
                constants={"new": {"_id": 60, "z": 1}},
            )
        ),
    ),
    ("delete collation not object", bw(dele({"a": 1}, collation=5))),
    ("delete hint number", bw(dele({"a": 1}, hint=5))),
    ("nsInfo collectionUUID not uuid", bw(ins({"_id": 20}), ns=[{"ns": C, "collectionUUID": 5}])),
    (
        "update error then more unordered",
        bw(upd({"_id": 1}, {"$nope": 1}), ins({"_id": 20}), dele({"_id": 0}), ordered=False),
    ),
    ("update error ordered", bw(ins({"_id": 20}), upd({"_id": 1}, {"$nope": 1}), dele({"_id": 0}))),
    (
        "upsert unique violation",
        bw(
            upd({"k": 1}, {"$set": {"u": 7}}, upsert=True),
            upd({"k": 2}, {"$set": {"u": 7}}, upsert=True),
        ),
    ),
    (
        "insert unique violation errInfo",
        bw(ins({"_id": 20, "u": 1}), ins({"_id": 21, "u": 1}), ordered=False),
    ),
    ("bulkWrite value 0", {"bulkWrite": 0, "ops": [ins({"_id": 20})], "nsInfo": NS}),
    ("bulkWrite value string", {"bulkWrite": "x", "ops": [ins({"_id": 20})], "nsInfo": NS}),
]


TYPE_LIST = re.compile(r"\[([a-zA-Z]+(?:, [a-zA-Z]+)+)\]")


def typed(v):
    if isinstance(v, ObjectId):
        return "<oid>"
    if isinstance(v, str):
        # A wrong-type error's expected-type list is rendered in a different
        # order by different mongod builds; compare it as the set it is.
        return TYPE_LIST.sub(lambda m: "[" + ", ".join(sorted(m.group(1).split(", "))) + "]", v)
    if isinstance(v, bool) or v is None:
        return v
    if isinstance(v, Int64):
        return f"long:{int(v)}"
    if isinstance(v, int):
        return f"int:{v}"
    if isinstance(v, float):
        return f"double:{v}"
    if isinstance(v, dict):
        return {k: typed(x) for k, x in v.items() if k not in NOISE}
    if isinstance(v, (list, tuple)):
        return [typed(x) for x in v]
    return repr(v)


def reset(client):
    client.drop_database(DB)
    for name in ("bwprobe",):
        client["local"][name].drop()
        client["config"][name].drop()
    db = client[DB]
    db.c.insert_many(
        [{"_id": i, "a": i, "s": "abB"[i], "arr": [i, i + 1, i + 2]} for i in range(3)]
    )
    db.c.create_index("a")
    db.c.create_index("u", unique=True, sparse=True)
    db.create_collection("d", validator={"v": {"$type": "int"}})
    db.d.insert_one({"_id": 0, "v": 1})
    db.create_collection("cap", capped=True, size=4096)
    db.cap.insert_one({"_id": 0})
    db.command({"create": "vw", "viewOn": "c", "pipeline": []})


def contents(client):
    out = {}
    for name in ("c", "d", "cap", "fresh"):
        docs = [typed(d) for d in client[DB][name].find({})]
        out[name] = sorted(docs, key=repr)
    return out


def one(client, command, database):
    reset(client)
    try:
        reply = dict(client[database].command(command))
        cursor = reply.get("cursor")
        if isinstance(cursor, dict) and cursor.get("id"):
            more = client["admin"].command(
                {"getMore": cursor["id"], "collection": "$cmd.bulkWrite"}
            )
            reply["cursor"] = {
                **cursor,
                "id": "<open>",
                "nextBatch": more["cursor"]["nextBatch"],
                "nextId": more["cursor"]["id"],
            }
        outcome = ("OK", typed(reply))
    except PyMongoError as exc:
        details = typed(dict(getattr(exc, "details", None) or {"errmsg": str(exc)[:300]}))
        details.pop("ok", None)
        outcome = ("ERR", details)
    return outcome, contents(client)


def run(client):
    results = {}
    for scenario in SCENARIOS:
        label, command = scenario[0], scenario[1]
        database = scenario[2] if len(scenario) > 2 else "admin"
        results[label] = one(client, command, database)
    return results


def self_check(expected):
    """The probe is worthless if the plain insert does not work on mongod."""
    outcome, after = expected["insert one"]
    if outcome[0] != "OK" or {"_id": "int:10", "a": "int:1"} not in after["c"]:
        sys.exit(f"SELF-CHECK FAILED on the reference server: {outcome} {after['c']}")


def main():
    with probe_targets() as (mon, targets):
        divergent = {label: 0 for label, _ in targets}
        expected = run(mon)
        self_check(expected)
        got = {name: run(cli) for name, cli in targets}
        for scenario in SCENARIOS:
            label = scenario[0]
            off = {name for name, g in got.items() if g[label] != expected[label]}
            if not off:
                continue
            for name in off:
                divergent[name] += 1
            print(f"  {label}")
            print(f"    mongod  : {expected[label][0]}")
            for name, g in got.items():
                mark = "   <-- diverges" if name in off else ""
                print(f"    {name:8s}: {g[label][0]}{mark}")
                if name in off and g[label][1] != expected[label][1]:
                    print(f"      WRITTEN mongod  : {expected[label][1]}")
                    print(f"      WRITTEN {name:8s}: {g[label][1]}")
        return report("bulk_write_command", len(SCENARIOS), divergent)


if __name__ == "__main__":
    sys.exit(main())
