"""Wide wrong-type sweep that compares MESSAGES, not just codes.

`arg_types_extended.py` compares ``(ok, code)``. That is how five Python-server
cases were once reported correct while carrying mongod's code with different
wording, and it is why this file exists. Two axes are widened here:

  * **the comparison** -- ``(code, errmsg)``, so a right code with wrong text is
    a finding rather than a pass;
  * **the corpus** -- the slots `arg_types_extended.py` never reached: insert /
    count / dropIndexes / renameCollection / listCollections / getMore /
    killCursors, the option slots on already-covered commands (hint, comment,
    collation, arrayFilters, ordered, capped, expireAfterSeconds, ...), and more
    aggregation stage specs.

**The corpus is still the coverage.** Widen it rather than trusting its count.

Run it the same way as the other probes; ``PROBE_SERVER`` points at an
already-running server (this is how the Rust server is swept), otherwise an
embedded Python server is started.

Findings are bucketed, because they are not all the same kind of thing:

  CODE   -- different error code (or one server errors and the other does not)
  MSG    -- same code, different message text
  UNSUP  -- we answer CommandNotFound (59); a missing command, not a wrong-type
            defect. Reported separately so it cannot inflate the real count.
"""

import datetime
import os
import re
import tempfile

import pymongo
from bson import Decimal128, Int64, ObjectId

from secantus import SecantusDBServer

OID = ObjectId("64b7f9a2c1d2e3f4a5b6c7d8")
WHEN = datetime.datetime(2026, 1, 2, 3, 4, 5)

MONGOD = os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27041")
SERVER = os.environ.get("PROBE_SERVER")
ONLY = os.environ.get("PROBE_ONLY")  # substring filter on the case label

DOCISH = [5, "x", True, None, 1.5, -1, Decimal128("2"), OID, WHEN]
NUMISH = [{}, "x", [1], None, True, OID, WHEN]
STRISH = [5, {}, [1], None, True, 1.5, OID, WHEN]
BOOLISH = [{}, [1], None, "x", 1.5, OID, WHEN]
ARRISH = [5, "x", True, None, {}, 1.5, OID, WHEN]


def cases():
    # ---- document-valued option slots not covered by the extended sweep ----
    for b in DOCISH:
        yield (f"find.hint={b!r}", {"find": "c", "hint": b})
        yield (f"find.readConcern={b!r}", {"find": "c", "readConcern": b})
        yield (f"count.query={b!r}", {"count": "c", "query": b})
        yield (f"count.hint={b!r}", {"count": "c", "hint": b})
        yield (f"distinct.query={b!r}", {"distinct": "c", "key": "a", "query": b})
        yield (f"distinct.collation={b!r}", {"distinct": "c", "key": "a", "collation": b})
        yield (f"insert.writeConcern={b!r}", {"insert": "c", "documents": [{}], "writeConcern": b})
        yield (
            f"update.let={b!r}",
            {"update": "c", "updates": [{"q": {}, "u": {"$set": {"a": 1}}}], "let": b},
        )
        yield (
            f"update.collation={b!r}",
            {"update": "c", "updates": [{"q": {}, "u": {"$set": {"a": 1}}, "collation": b}]},
        )
        yield (
            f"update.hint={b!r}",
            {"update": "c", "updates": [{"q": {}, "u": {"$set": {"a": 1}}, "hint": b}]},
        )
        yield (
            f"delete.collation={b!r}",
            {"delete": "c", "deletes": [{"q": {}, "limit": 0, "collation": b}]},
        )
        yield (f"delete.hint={b!r}", {"delete": "c", "deletes": [{"q": {}, "limit": 0, "hint": b}]})
        yield (
            f"fam.fields={b!r}",
            {"findAndModify": "c", "query": {}, "update": {"$set": {"a": 1}}, "fields": b},
        )
        yield (
            f"fam.sort={b!r}",
            {"findAndModify": "c", "query": {}, "update": {"$set": {"a": 1}}, "sort": b},
        )
        yield (
            f"fam.collation={b!r}",
            {"findAndModify": "c", "query": {}, "update": {"$set": {"a": 1}}, "collation": b},
        )
        yield (
            f"fam.let={b!r}",
            {"findAndModify": "c", "query": {}, "update": {"$set": {"a": 1}}, "let": b},
        )
        yield (
            f"fam.hint={b!r}",
            {"findAndModify": "c", "query": {}, "update": {"$set": {"a": 1}}, "hint": b},
        )
        yield (
            f"agg.collation={b!r}",
            {"aggregate": "c", "pipeline": [], "cursor": {}, "collation": b},
        )
        yield (f"agg.hint={b!r}", {"aggregate": "c", "pipeline": [], "cursor": {}, "hint": b})
        yield (
            f"agg.readConcern={b!r}",
            {"aggregate": "c", "pipeline": [], "cursor": {}, "readConcern": b},
        )
        yield (f"listCollections.filter={b!r}", {"listCollections": 1, "filter": b})
        yield (f"listCollections.cursor={b!r}", {"listCollections": 1, "cursor": b})
        yield (
            f"createIndexes.partialFilter={b!r}",
            {
                "createIndexes": "c",
                "indexes": [{"key": {"a": 1}, "name": "i", "partialFilterExpression": b}],
            },
        )
        yield (
            f"createIndexes.collation={b!r}",
            {"createIndexes": "c", "indexes": [{"key": {"a": 1}, "name": "i", "collation": b}]},
        )
        yield (
            f"create.validator={b!r}",
            {"create": f"v_{abs(hash(repr(b))) % 10000}", "validator": b},
        )
        yield (
            f"create.timeseries={b!r}",
            {"create": f"ts_{abs(hash(repr(b))) % 10000}", "timeseries": b},
        )
        yield (f"collMod.validator={b!r}", {"collMod": "c", "validator": b})
        yield (f"collMod.preImages={b!r}", {"collMod": "c", "changeStreamPreAndPostImages": b})
        # aggregation stage specs the extended sweep did not reach
        yield (
            f"project.spec={b!r}",
            {"aggregate": "c", "pipeline": [{"$project": b}], "cursor": {}},
        )
        yield (
            f"addFields.spec={b!r}",
            {"aggregate": "c", "pipeline": [{"$addFields": b}], "cursor": {}},
        )
        yield (
            f"replaceRoot.spec={b!r}",
            {"aggregate": "c", "pipeline": [{"$replaceRoot": b}], "cursor": {}},
        )
        yield (f"facet.spec={b!r}", {"aggregate": "c", "pipeline": [{"$facet": b}], "cursor": {}})
        yield (f"bucket.spec={b!r}", {"aggregate": "c", "pipeline": [{"$bucket": b}], "cursor": {}})
        yield (
            f"sortByCount.spec={b!r}",
            {"aggregate": "c", "pipeline": [{"$sortByCount": b}], "cursor": {}},
        )
        yield (
            f"geoNear.spec={b!r}",
            {"aggregate": "c", "pipeline": [{"$geoNear": b}], "cursor": {}},
        )
        yield (
            f"graphLookup.spec={b!r}",
            {"aggregate": "c", "pipeline": [{"$graphLookup": b}], "cursor": {}},
        )
        yield (
            f"unionWith.spec={b!r}",
            {"aggregate": "c", "pipeline": [{"$unionWith": b}], "cursor": {}},
        )
        yield (
            f"setWindowFields.spec={b!r}",
            {"aggregate": "c", "pipeline": [{"$setWindowFields": b}], "cursor": {}},
        )
        yield (
            f"densify.spec={b!r}",
            {"aggregate": "c", "pipeline": [{"$densify": b}], "cursor": {}},
        )
        yield (f"fill.spec={b!r}", {"aggregate": "c", "pipeline": [{"$fill": b}], "cursor": {}})
        yield (f"sample.spec={b!r}", {"aggregate": "c", "pipeline": [{"$sample": b}], "cursor": {}})
        yield (f"redact.spec={b!r}", {"aggregate": "c", "pipeline": [{"$redact": b}], "cursor": {}})

    # ---- array-valued slots ----
    for b in ARRISH:
        yield (f"insert.documents={b!r}", {"insert": "c", "documents": b})
        yield (f"update.updates={b!r}", {"update": "c", "updates": b})
        yield (f"delete.deletes={b!r}", {"delete": "c", "deletes": b})
        yield (f"aggregate.pipeline={b!r}", {"aggregate": "c", "pipeline": b, "cursor": {}})
        yield (
            f"fam.arrayFilters={b!r}",
            {
                "findAndModify": "c",
                "query": {},
                "update": {"$set": {"a.$[e]": 1}},
                "arrayFilters": b,
            },
        )
        yield (
            f"update.arrayFilters={b!r}",
            {
                "update": "c",
                "updates": [{"q": {}, "u": {"$set": {"a.$[e]": 1}}, "arrayFilters": b}],
            },
        )
        yield (f"killCursors.cursors={b!r}", {"killCursors": "c", "cursors": b})

    # ---- numeric slots ----
    for b in NUMISH:
        yield (f"count.limit={b!r}", {"count": "c", "limit": b})
        yield (f"count.skip={b!r}", {"count": "c", "skip": b})
        yield (f"count.maxTimeMS={b!r}", {"count": "c", "maxTimeMS": b})
        yield (
            f"agg.maxTimeMS={b!r}",
            {"aggregate": "c", "pipeline": [], "cursor": {}, "maxTimeMS": b},
        )
        yield (f"distinct.maxTimeMS={b!r}", {"distinct": "c", "key": "a", "maxTimeMS": b})
        yield (
            f"fam.maxTimeMS={b!r}",
            {"findAndModify": "c", "query": {}, "update": {"$set": {"a": 1}}, "maxTimeMS": b},
        )
        yield (f"getMore.batchSize={b!r}", {"getMore": Int64(1), "collection": "c", "batchSize": b})
        yield (f"getMore.maxTimeMS={b!r}", {"getMore": Int64(1), "collection": "c", "maxTimeMS": b})
        yield (
            f"createIndexes.expireAfter={b!r}",
            {
                "createIndexes": "c",
                "indexes": [{"key": {"a": 1}, "name": "i", "expireAfterSeconds": b}],
            },
        )
        yield (
            f"create.size={b!r}",
            {"create": f"sz_{abs(hash(repr(b))) % 10000}", "capped": True, "size": b},
        )
        yield (
            f"create.max={b!r}",
            {"create": f"mx_{abs(hash(repr(b))) % 10000}", "capped": True, "size": 4096, "max": b},
        )
        yield (
            f"listCollections.batchSize={b!r}",
            {"listCollections": 1, "cursor": {"batchSize": b}},
        )
        yield (f"listIndexes.batchSize={b!r}", {"listIndexes": "c", "cursor": {"batchSize": b}})

    # ---- string slots ----
    for b in STRISH:
        yield (f"getMore.collection={b!r}", {"getMore": Int64(1), "collection": b})
        yield (f"dropIndexes.index={b!r}", {"dropIndexes": "c", "index": b})
        yield (f"renameCollection.to={b!r}", {"renameCollection": "argtypes.c", "to": b})
        yield (f"find.comment={b!r}", {"find": "c", "comment": b})
        yield (
            f"sortByCount.str={b!r}",
            {"aggregate": "c", "pipeline": [{"$sortByCount": b}], "cursor": {}},
        )
        yield (f"collMod.viewOn={b!r}", {"collMod": "c", "viewOn": b})

    # ---- boolean slots ----
    for b in BOOLISH:
        yield (f"insert.ordered={b!r}", {"insert": "c", "documents": [{}], "ordered": b})
        yield (
            f"update.ordered={b!r}",
            {"update": "c", "updates": [{"q": {}, "u": {"$set": {"a": 1}}}], "ordered": b},
        )
        yield (
            f"delete.ordered={b!r}",
            {"delete": "c", "deletes": [{"q": {}, "limit": 0}], "ordered": b},
        )
        yield (
            f"insert.bypass={b!r}",
            {"insert": "c", "documents": [{}], "bypassDocumentValidation": b},
        )
        yield (f"fam.remove={b!r}", {"findAndModify": "c", "query": {}, "remove": b})
        yield (
            f"fam.new={b!r}",
            {"findAndModify": "c", "query": {}, "update": {"$set": {"a": 1}}, "new": b},
        )
        yield (f"find.tailable={b!r}", {"find": "c", "tailable": b})
        yield (f"find.awaitData={b!r}", {"find": "c", "tailable": True, "awaitData": b})
        yield (f"find.showRecordId={b!r}", {"find": "c", "showRecordId": b})
        yield (f"find.returnKey={b!r}", {"find": "c", "returnKey": b})
        yield (f"find.allowDiskUse={b!r}", {"find": "c", "allowDiskUse": b})
        yield (
            f"agg.allowDiskUse={b!r}",
            {"aggregate": "c", "pipeline": [], "cursor": {}, "allowDiskUse": b},
        )
        yield (
            f"createIndexes.unique={b!r}",
            {"createIndexes": "c", "indexes": [{"key": {"a": 1}, "name": "i", "unique": b}]},
        )
        yield (
            f"createIndexes.sparse={b!r}",
            {"createIndexes": "c", "indexes": [{"key": {"a": 1}, "name": "i", "sparse": b}]},
        )
        yield (
            f"create.capped={b!r}",
            {"create": f"cp_{abs(hash(repr(b))) % 10000}", "capped": b, "size": 4096},
        )
        yield (
            f"renameCollection.dropTarget={b!r}",
            {"renameCollection": "argtypes.c", "to": "argtypes.c2", "dropTarget": b},
        )


#: Sentinel for a shape pymongo refuses to ENCODE, so it never reaches the wire.
#: ``insert``/``update``/``delete`` promote their array argument to an OP_MSG
#: document sequence, and a non-iterable value fails in the driver. Those shapes
#: are unreachable through the conformance target and are not server findings.
CLIENT = "<client-side encode failure>"


def run(cli, dbn, cmd):
    db = cli[dbn]
    db.c.drop()
    db.c.insert_one({"_id": 1, "a": 1})
    try:
        r = db.command(dict(cmd))
        we = (r.get("writeErrors") or [{}])[0]
        if we:
            return (we.get("code"), we.get("errmsg") or "")
        return (None, "")
    except pymongo.errors.PyMongoError as e:
        return (getattr(e, "code", None), _msg(e))
    except (TypeError, ValueError, AttributeError):
        return (CLIENT, CLIENT)


def _msg(e):
    d = getattr(e, "details", None)
    if isinstance(d, dict) and d.get("errmsg"):
        return d["errmsg"]
    return str(e)


def norm(msg, dbn):
    """Normalise a message so only real differences survive.

    Two things are deliberately erased. The per-case database name, so labels
    line up. And the ORDER of an expected-type list: mongod renders
    ``'[decimal, int, double, long]'`` in a different order on 8.2.1 than on
    8.2.11 -- a PATCH bump -- so the order pins a build rather than a behaviour.
    `tests/test_mongod_differential.py` compares that list as the set it is, and
    so does this.
    """
    if not msg:
        return ""
    m = msg.replace(dbn, "<db>")
    m = re.sub(r"\b\d{4}-\d{2}-\d{2}T[\d:.+-]+\b", "<ts>", m)
    m = re.sub(
        r"'\[([a-zA-Z, ]+)\]'",
        lambda g: "'[" + ", ".join(sorted(t.strip() for t in g.group(1).split(","))) + "]'",
        m,
    )
    return m.strip()


def main():
    if SERVER:
        srv = None
        sec = pymongo.MongoClient(SERVER, directConnection=True, serverSelectionTimeoutMS=8000)
        print(f"  server under test: {SERVER}")
    else:
        d = tempfile.mkdtemp()
        srv = SecantusDBServer(port=0, storage_path=d)
        srv.start()
        sec = pymongo.MongoClient(
            f"mongodb://{srv.address[0]}:{srv.address[1]}", directConnection=True
        )
        print("  server under test: embedded Python SecantusDBServer")
    mon = pymongo.MongoClient(MONGOD, directConnection=True, serverSelectionTimeoutMS=8000)

    code_diffs, msg_diffs, unsup, client_skips, total = [], [], [], [], 0
    for i, (label, cmd) in enumerate(cases()):
        if ONLY and ONLY not in label:
            continue
        total += 1
        dbn = f"y{i}"
        mc, mm = run(mon, dbn, cmd)
        oc, om = run(sec, dbn, cmd)
        mm, om = norm(mm, dbn), norm(om, dbn)
        if mc == CLIENT or oc == CLIENT:
            client_skips.append(label)
        elif oc == 59 and mc != 59:
            unsup.append((label, mc, mm))
        elif mc != oc:
            code_diffs.append((label, (mc, mm), (oc, om)))
        elif mm != om:
            msg_diffs.append((label, mm, om))

    print(
        f"  cases: {total}   CODE: {len(code_diffs)}   MSG: {len(msg_diffs)}   "
        f"UNSUP(59): {len(unsup)}   unsendable-by-pymongo: {len(client_skips)}\n"
    )
    if code_diffs:
        print("  === CODE differences ===")
        for label, m, o in code_diffs:
            print(f"    {label}")
            print(f"        mongod   {m[0]}  {m[1][:150]}")
            print(f"        ours     {o[0]}  {o[1][:150]}")
    if msg_diffs:
        print("\n  === MESSAGE differences (code matches) ===")
        for label, m, o in msg_diffs:
            print(f"    {label}")
            print(f"        mongod   {m[:150]}")
            print(f"        ours     {o[:150]}")
    if unsup:
        print("\n  === command not supported here (59) ===")
        for label, mc, mm in unsup:
            print(f"    {label:<44} mongod={mc} {mm[:80]}")
    sec.close()
    if srv is not None:
        srv.stop()


main()
