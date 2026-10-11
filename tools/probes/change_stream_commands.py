"""Differential-probe change streams, command by command, against a real mongod.

What `aggregate` accepts as a `$changeStream`, the shape of every event the
stream delivers, `getMore` on it, the invalidating operations, resuming by
`resumeAfter` / `startAfter` / `startAtOperationTime`, the database and
cluster scopes, pre- and post-images, and what a transaction's events carry.
Commands go over a raw connection so a cursor id, a resume token and a
session are handed on exactly as they came back.

mongod needs a replica set (`mongod --replSet rs0`, then `replSetInitiate`).

    PROBE_MONGOD=mongodb://127.0.0.1:27441 \\
    PROBE_SERVER=mongodb://127.0.0.1:27442 python tools/probes/change_stream_commands.py

`PROBE_ONLY=<substring>` runs the scenarios whose name contains it.
"""

import os
import re
import sys
import uuid

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from _wire import Wire  # noqa: E402
from bson import Binary, Int64, Timestamp  # noqa: E402
from bson.binary import UUID_SUBTYPE  # noqa: E402

DEFAULT_MONGOD = "mongodb://127.0.0.1:27441"
NOISE = ("$clusterTime", "operationTime", "opTime", "electionId", "localTime")

class Sid:
    """A session id. mongod remembers a session's transaction numbers, so
    each scenario run gets its own, salted by run."""

    def __init__(self, label="s"):
        self.label = label

    def __repr__(self):
        return f"<lsid {self.label}>"


def lsid(label="s"):
    return Sid(label)


class Cur:
    """The id of the cursor an earlier step saved under `label`."""

    def __init__(self, label="k"):
        self.label = label

    def __repr__(self):
        return f"<cursor {self.label}>"


class Tok:
    """The resume token (`_id`) of event `n` collected under `label`."""

    def __init__(self, n, label="k"):
        self.n = n
        self.label = label

    def __repr__(self):
        return f"<token {self.label}[{self.n}]>"


class Pbrt:
    """The `postBatchResumeToken` of the last batch read under `label`."""

    def __init__(self, label="k"):
        self.label = label

    def __repr__(self):
        return f"<pbrt {self.label}>"


class OpTime:
    """The `operationTime` of the last reply."""

    def __repr__(self):
        return "<operationTime>"


def watch(*stages, coll="c", batch=None, save="k", **spec):
    cursor = {} if batch is None else {"batchSize": batch}
    return {
        "aggregate": coll,
        "pipeline": [{"$changeStream": spec}, *stages],
        "cursor": cursor,
        "save": save,
    }


def more(label="k", coll="c", **kw):
    return {"getMore": Cur(label), "collection": coll, "maxTimeMS": 400, "save": label, **kw}


def insert(*docs, coll="c", **kw):
    return {"insert": coll, "documents": list(docs), **kw}


def update(q, u, coll="c", **kw):
    return {"update": coll, "updates": [{"q": q, "u": u, **kw}]}


def delete(q, coll="c"):
    return {"delete": coll, "deletes": [{"q": q, "limit": 1}]}


S: list[tuple[str, list]] = []


def scenario(name, *steps):
    S.append((name, list(steps)))


# --- what aggregate accepts --------------------------------------------------

scenario("open on an empty namespace", watch(), more())
scenario("stage not first", {"aggregate": "c", "pipeline": [{"$match": {}}, {"$changeStream": {}}], "cursor": {}})
scenario("spec not a document", {"aggregate": "c", "pipeline": [{"$changeStream": 1}], "cursor": {}})
scenario("unknown field", watch(bogus=1))
scenario("fullDocument bad value", watch(fullDocument="everything"))
scenario("fullDocument wrong type", watch(fullDocument=1))
scenario("fullDocumentBeforeChange bad value", watch(fullDocumentBeforeChange="always"))
scenario("showExpandedEvents wrong type", watch(showExpandedEvents=1))
scenario("resumeAfter not a document", watch(resumeAfter="abc"))
scenario("resumeAfter _data not a string", watch(resumeAfter={"_data": 1}))
scenario("resumeAfter bad hex", watch(resumeAfter={"_data": "zz"}))
scenario("resumeAfter unknown key", watch(resumeAfter={"_data": "82", "x": 1}))
scenario("resumeAfter and startAfter", watch(resumeAfter={"_data": "82"}, startAfter={"_data": "82"}))
scenario("resumeAfter and startAtOperationTime", watch(resumeAfter={"_data": "82"}, startAtOperationTime=Timestamp(1, 1)))
scenario("startAtOperationTime wrong type", watch(startAtOperationTime=5))
scenario("allChangesForCluster on a collection", watch(allChangesForCluster=True))
scenario("allChangesForCluster on a database", {"aggregate": 1, "pipeline": [{"$changeStream": {"allChangesForCluster": True}}], "cursor": {}})
scenario("cluster on admin", ("admin", {"aggregate": 1, "pipeline": [{"$changeStream": {"allChangesForCluster": True}}], "cursor": {}, "save": "k"}), ("admin", {"getMore": Cur(), "collection": "$cmd.aggregate", "maxTimeMS": 300}))
scenario("database scope opens", {"aggregate": 1, "pipeline": [{"$changeStream": {}}], "cursor": {}, "save": "k"}, insert({"_id": 1}), insert({"_id": 1}, coll="d"), {"getMore": Cur(), "collection": "$cmd.aggregate", "maxTimeMS": 400})
scenario("on the admin database", ("admin", watch()))
scenario("on the local database", ("local", watch()))
scenario("on the config database", ("config", watch()))
scenario("on system.views", watch(coll="system.views"))
scenario("on system.profile", watch(coll="system.profile"))
scenario("on a view", {"create": "v", "viewOn": "c", "pipeline": []}, watch(coll="v"))
scenario("on a time-series collection", {"create": "ts", "timeseries": {"timeField": "t"}}, watch(coll="ts"))
scenario("with a collation", watch(collation={"locale": "en"}), more())
scenario("with a $sort after it", watch({"$sort": {"_id": 1}}))
scenario("with a $group after it", watch({"$group": {"_id": "$operationType"}}))
scenario("with a $limit after it", watch({"$limit": 1}))
scenario("with $lookup after it", watch({"$lookup": {"from": "d", "localField": "a", "foreignField": "a", "as": "j"}}))
scenario("with $out after it", watch({"$out": "o"}))
scenario("with $match $project $addFields", watch({"$match": {"operationType": "insert"}}, {"$project": {"fullDocument": 1}}, {"$addFields": {"x": 1}}), insert({"_id": 1}), update({"_id": 1}, {"$set": {"a": 1}}), more())
scenario("projecting out _id", watch({"$project": {"_id": 0}}), insert({"_id": 1}), more())
scenario("replacing _id", watch({"$addFields": {"_id": 7}}), insert({"_id": 1}), more())
scenario("$replaceRoot off the event", watch({"$replaceRoot": {"newRoot": "$fullDocument"}}), insert({"_id": 1}), more())
scenario("batchSize zero", watch(batch=0), insert({"_id": 1}), more())
scenario("batchSize one", watch(batch=1), insert({"_id": 1}, {"_id": 2}, {"_id": 3}), more(batchSize=1), more(batchSize=1), more())
scenario("explain", {"aggregate": "c", "pipeline": [{"$changeStream": {}}], "cursor": {}, "explain": True})
scenario("readConcern majority", watch(readConcern={"level": "majority"}), more())
scenario("readConcern linearizable", watch(readConcern={"level": "linearizable"}))
scenario("readConcern snapshot", watch(readConcern={"level": "snapshot"}))

# --- getMore ---------------------------------------------------------------

scenario("getMore waits and answers empty", watch(), more(), more())
scenario("getMore on another collection name", watch(), {"getMore": Cur(), "collection": "d"})
scenario("getMore batchSize zero", watch(), insert({"_id": 1}), more(batchSize=0))
scenario("getMore without maxTimeMS", watch(), insert({"_id": 1}), {"getMore": Cur(), "collection": "c"})
scenario("getMore maxTimeMS zero", watch(), insert({"_id": 1}), more(maxTimeMS=0))
scenario("killCursors", watch(), {"killCursors": "c", "cursors": [Cur()]}, more())
scenario("getMore after the stream ends", watch(), {"drop": "c"}, more(), more(), more())

# --- events -----------------------------------------------------------------

scenario("insert", watch(), insert({"_id": 1, "a": [1, {"b": 2}], "s": "x"}), more())
scenario("insert with document _id", watch(), insert({"_id": {"k": 1, "j": "a"}, "a": 1}), more())
scenario("insert many in one batch", watch(), insert({"_id": 1}, {"_id": 2}, {"_id": 3}), more())
scenario("insert with a write error", watch(), insert({"_id": 1}), insert({"_id": 1}, {"_id": 2}, ordered=False), more())
scenario("update", watch(), insert({"_id": 1, "a": 1}), update({"_id": 1}, {"$set": {"b": 2}, "$unset": {"a": ""}}), more())
scenario("update no change", watch(), insert({"_id": 1, "a": 1}), update({"_id": 1}, {"$set": {"a": 1}}), more())
scenario("update lookup", watch(fullDocument="updateLookup"), insert({"_id": 1, "a": 1}), update({"_id": 1}, {"$set": {"a": 2}}), more())
scenario("update lookup after delete", watch(fullDocument="updateLookup", batch=0), insert({"_id": 1, "a": 1}), update({"_id": 1}, {"$set": {"a": 2}}), delete({"_id": 1}), more())
scenario("update whenAvailable required", watch(fullDocument="whenAvailable"), insert({"_id": 1}), update({"_id": 1}, {"$set": {"a": 2}}), more(), watch(fullDocument="required", save="r"), update({"_id": 1}, {"$set": {"a": 3}}), delete({"_id": 1}), more("r"))
scenario("replace", watch(), insert({"_id": 1, "a": 1}), update({"_id": 1}, {"b": 2}), more())
scenario("replace with lookup", watch(fullDocument="updateLookup"), insert({"_id": 1, "a": 1}), update({"_id": 1}, {"b": 2}), more())
scenario("upsert", watch(), update({"_id": 9}, {"$set": {"a": 1}}, upsert=True), update({"_id": 8}, {"a": 1}, upsert=True), more())
scenario("delete", watch(), insert({"_id": 1, "a": 1}), delete({"_id": 1}), more())
scenario("multi update and delete many", watch(), insert({"_id": 1}, {"_id": 2}), {"update": "c", "updates": [{"q": {}, "u": {"$set": {"m": 1}}, "multi": True}]}, {"delete": "c", "deletes": [{"q": {}, "limit": 0}]}, more())
scenario("findAndModify", watch(), insert({"_id": 1, "a": 1}), {"findAndModify": "c", "query": {"_id": 1}, "update": {"$inc": {"a": 1}}}, {"findAndModify": "c", "query": {"_id": 1}, "remove": True}, more())
scenario("pipeline update", watch(), insert({"_id": 1, "a": 1}), update({"_id": 1}, [{"$set": {"b": {"$add": ["$a", 1]}}}]), more())
scenario("array changes", watch(showExpandedEvents=True), insert({"_id": 1, "arr": [1, 2, 3]}), update({"_id": 1}, {"$pop": {"arr": 1}}), update({"_id": 1}, {"$set": {"arr.0": 9}}), more())
scenario("drop", watch(), insert({"_id": 1}), {"drop": "c"}, more(), more())
scenario("drop other collection", watch(), insert({"_id": 1}, coll="d"), {"drop": "d"}, insert({"_id": 1}), more())
scenario("rename", watch(), insert({"_id": 1}), ("admin", {"renameCollection": "@db.c", "to": "@db.d"}), more(), more())
scenario("rename onto it", watch(), insert({"_id": 1}, coll="d"), ("admin", {"renameCollection": "@db.d", "to": "@db.c", "dropTarget": True}), more(), more())
scenario("dropDatabase", watch(), insert({"_id": 1}), {"dropDatabase": 1}, more(), more())
scenario("create and index events hidden", watch(), {"create": "c"}, {"createIndexes": "c", "indexes": [{"key": {"a": 1}, "name": "a_1"}]}, {"dropIndexes": "c", "index": "a_1"}, insert({"_id": 1}), more())
scenario("create event expanded", watch(showExpandedEvents=True), {"create": "c", "capped": True, "size": 4096}, more())
scenario("createIndexes event expanded", watch(showExpandedEvents=True), {"createIndexes": "c", "indexes": [{"key": {"a": 1}, "name": "a_1", "unique": True}]}, {"dropIndexes": "c", "index": "a_1"}, more())
scenario("collMod event expanded", watch(showExpandedEvents=True), {"create": "c"}, {"collMod": "c", "validator": {"a": {"$gt": 0}}}, {"collMod": "c", "changeStreamPreAndPostImages": {"enabled": True}}, more())
scenario("expanded update", watch(showExpandedEvents=True), insert({"_id": 1, "a": {"b": 1}}), update({"_id": 1}, {"$set": {"a.b": 2}}), more())
scenario("documentKey order", watch(), insert({"_id": 1, "z": 1, "a": 1}), update({"_id": 1}, {"$set": {"z": 2}}), more())
scenario("split large event", watch({"$changeStreamSplitLargeEvent": {}}), insert({"_id": 1, "a": 1}), update({"_id": 1}, {"$set": {"a": 2}}), more())
scenario("split stage not last", watch({"$changeStreamSplitLargeEvent": {}}, {"$match": {}}))
scenario("split stage without change stream", {"aggregate": "c", "pipeline": [{"$changeStreamSplitLargeEvent": {}}], "cursor": {}})

# --- pre-images -----------------------------------------------------------

PRE = {"create": "c", "changeStreamPreAndPostImages": {"enabled": True}}
scenario("pre-image on update and delete", PRE, watch(fullDocumentBeforeChange="whenAvailable"), insert({"_id": 1, "a": 1}), update({"_id": 1}, {"$set": {"a": 2}}), delete({"_id": 1}), more())
scenario("pre-image required", PRE, watch(fullDocumentBeforeChange="required"), insert({"_id": 1, "a": 1}), update({"_id": 1}, {"$set": {"a": 2}}), more())
scenario("pre-image required without images", watch(fullDocumentBeforeChange="required"), insert({"_id": 1, "a": 1}), update({"_id": 1}, {"$set": {"a": 2}}), more())
scenario("pre-image whenAvailable without images", watch(fullDocumentBeforeChange="whenAvailable"), insert({"_id": 1, "a": 1}), update({"_id": 1}, {"$set": {"a": 2}}), more())
scenario("pre-image off", PRE, watch(fullDocumentBeforeChange="off"), insert({"_id": 1, "a": 1}), update({"_id": 1}, {"$set": {"a": 2}}), more())
scenario("pre-image on replace", PRE, watch(fullDocumentBeforeChange="required", fullDocument="updateLookup"), insert({"_id": 1, "a": 1}), update({"_id": 1}, {"b": 1}), more())
scenario("pre-image enabled later", watch(fullDocumentBeforeChange="whenAvailable"), insert({"_id": 1, "a": 1}), update({"_id": 1}, {"$set": {"a": 2}}), {"collMod": "c", "changeStreamPreAndPostImages": {"enabled": True}}, update({"_id": 1}, {"$set": {"a": 3}}), more())

# --- resuming -------------------------------------------------------------

THREE = [insert({"_id": 1}), insert({"_id": 2}), insert({"_id": 3})]
scenario("resumeAfter an event", watch(), *THREE, more(), watch(resumeAfter=Tok(0), save="r"), more("r"))
scenario("resumeAfter the last event", watch(), *THREE, more(), watch(resumeAfter=Tok(2), save="r"), more("r"))
scenario("startAfter an event", watch(), *THREE, more(), watch(startAfter=Tok(1), save="r"), more("r"))
scenario("resumeAfter postBatchResumeToken", watch(), more(), *THREE, watch(resumeAfter=Pbrt(), save="r"), more("r"))
scenario("resume token from a getMore batch", watch(batch=0), *THREE, more(batchSize=1), more(batchSize=1), watch(resumeAfter=Pbrt(), save="r"), more("r"))
scenario("resumeAfter invalidate", watch(), insert({"_id": 1}), {"drop": "c"}, more(), more(), watch(resumeAfter=Tok(2), save="r"))
scenario("startAfter invalidate", watch(), insert({"_id": 1}), {"drop": "c"}, more(), more(), watch(startAfter=Tok(2), save="r"), insert({"_id": 5}), more("r"))
DB_END = [{"aggregate": 1, "pipeline": [{"$changeStream": {}}], "cursor": {}, "save": "k"}, insert({"_id": 1}), {"dropDatabase": 1}, {"getMore": Cur(), "collection": "$cmd.aggregate", "maxTimeMS": 400, "save": "k"}]
scenario("startAfter invalidate at database scope", *DB_END, {"aggregate": 1, "pipeline": [{"$changeStream": {"startAfter": Tok(3)}}], "cursor": {}, "save": "r"}, insert({"_id": 2}), {"getMore": Cur("r"), "collection": "$cmd.aggregate", "maxTimeMS": 400})
scenario("startAfter the dropDatabase event", *DB_END, {"aggregate": 1, "pipeline": [{"$changeStream": {"startAfter": Tok(2)}}], "cursor": {}, "save": "r"}, insert({"_id": 2}), {"getMore": Cur("r"), "collection": "$cmd.aggregate", "maxTimeMS": 400})
scenario("resume token of a projected event", watch({"$project": {"operationType": 1}}), insert({"_id": 1}), insert({"_id": 2}), more(), watch(resumeAfter=Tok(0), save="r"), more("r"))
scenario("resume token not in the oplog", watch(resumeAfter={"_data": "8200000001000000012B022C0100296E5A1004" + "00" * 16 + "463C5F6964003C6100000004"}))
scenario("resume token from another namespace", watch(), insert({"_id": 1}), more(), watch(coll="d", resumeAfter=Tok(0), save="r"), insert({"_id": 1}, coll="d"), more("r", coll="d"))
scenario("startAtOperationTime now", watch(), insert({"_id": 1}), more(), watch(startAtOperationTime=OpTime(), save="r"), insert({"_id": 2}), more("r"))
scenario("startAtOperationTime in the past", insert({"_id": 1}), {"drop": "c"}, watch(startAtOperationTime=Timestamp(1, 1)))
scenario("startAtOperationTime in the future", watch(startAtOperationTime=Timestamp(2**31 - 1, 1)), insert({"_id": 1}), more())
scenario("resume with a different pipeline", watch(), *THREE, more(), watch({"$match": {"documentKey._id": 3}}, resumeAfter=Tok(0), save="r"), more("r"))
scenario("resume after the collection is recreated", watch(), insert({"_id": 1}), {"drop": "c"}, more(), insert({"_id": 2}), watch(resumeAfter=Tok(0), save="r"), more("r"))

# --- scopes ---------------------------------------------------------------

DB_WATCH = {"aggregate": 1, "pipeline": [{"$changeStream": {}}], "cursor": {}, "save": "k"}
DB_MORE = {"getMore": Cur(), "collection": "$cmd.aggregate", "maxTimeMS": 400, "save": "k"}
scenario("database scope events", DB_WATCH, insert({"_id": 1}), insert({"_id": 1}, coll="d"), {"drop": "d"}, ("admin", {"renameCollection": "@db.c", "to": "@db.e"}), DB_MORE)
scenario("database scope ignores system and other dbs", DB_WATCH, {"create": "v", "viewOn": "c", "pipeline": []}, ("@other", insert({"_id": 1})), insert({"_id": 1}), DB_MORE)
scenario("database scope dropDatabase", DB_WATCH, insert({"_id": 1}), {"dropDatabase": 1}, DB_MORE, DB_MORE)
CL_WATCH = ("admin", {"aggregate": 1, "pipeline": [{"$changeStream": {"allChangesForCluster": True}}], "cursor": {}, "save": "k"})
CL_MORE = ("admin", {"getMore": Cur(), "collection": "$cmd.aggregate", "maxTimeMS": 400, "save": "k"})
scenario("cluster scope events", CL_WATCH, insert({"_id": 1}), ("@other", insert({"_id": 2})), {"dropDatabase": 1}, CL_MORE, CL_MORE)
scenario("cluster scope expanded", ("admin", {"aggregate": 1, "pipeline": [{"$changeStream": {"allChangesForCluster": True, "showExpandedEvents": True}}], "cursor": {}, "save": "k"}), {"create": "c"}, {"createIndexes": "c", "indexes": [{"key": {"a": 1}, "name": "a_1"}]}, CL_MORE)
scenario("cluster scope ignores admin local config", CL_WATCH, ("admin", insert({"_id": 1}, coll="x")), ("config", insert({"_id": 1}, coll="x")), insert({"_id": 1}), CL_MORE, ("admin", {"drop": "x"}), ("config", {"drop": "x"}))

# --- transactions and sessions ------------------------------------------

TX = {"lsid": lsid(), "txnNumber": Int64(1), "autocommit": False}
scenario("events from a transaction", watch(), {**insert({"_id": 1}), **TX, "startTransaction": True}, {**insert({"_id": 2}), **TX}, {**update({"_id": 1}, {"$set": {"a": 1}}), **TX}, ("admin", {"commitTransaction": 1, **TX}), more())
scenario("aborted transaction", watch(), {**insert({"_id": 1}), **TX, "startTransaction": True}, ("admin", {"abortTransaction": 1, **TX}), insert({"_id": 2}), more())
scenario("retryable write", watch(), {**insert({"_id": 1}), "lsid": lsid(), "txnNumber": Int64(5)}, {**insert({"_id": 1}), "lsid": lsid(), "txnNumber": Int64(5)}, more())
scenario("stream opened in a session", {**watch(), "lsid": lsid("w")}, insert({"_id": 1}), {**more(), "lsid": lsid("w")}, more())
scenario("stream opened in a transaction", {**watch(), **TX, "startTransaction": True})


# --- harness -----------------------------------------------------------------

HEX = re.compile(r"\b[0-9A-F]{24,}\b")
UUID_TEXT = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")


def resolve(value, db, state):
    if isinstance(value, Cur):
        return Int64(state["cursors"].get(value.label, 987654321))
    if isinstance(value, Tok):
        events = state["events"].get(value.label, [])
        if value.n < len(events) and isinstance(events[value.n].get("_id"), dict):
            return events[value.n]["_id"]
        return {"_data": "82"}
    if isinstance(value, Pbrt):
        return state["pbrt"].get(value.label, {"_data": "82"})
    if isinstance(value, Sid):
        seed = state["salt"] + value.label
        return {"id": Binary(uuid.uuid5(uuid.NAMESPACE_OID, seed).bytes, UUID_SUBTYPE)}
    if isinstance(value, OpTime):
        return state.get("optime", Timestamp(1, 1))
    if isinstance(value, dict):
        return {k: resolve(v, db, state) for k, v in value.items()}
    if isinstance(value, list):
        return [resolve(v, db, state) for v in value]
    if isinstance(value, str) and "@db" in value:
        return value.replace("@db", db)
    return value


def clean(value, key=None, db=""):
    if isinstance(value, dict):
        if set(value) <= {"_data", "_typeBits"} and "_data" in value:
            return "TOKEN"
        out = {}
        for k, v in value.items():
            if k in NOISE:
                continue
            if k == "id" and key == "cursor":
                out[k] = "CURSOR" if v else 0
            elif k in ("clusterTime", "wallTime") or (k == "ts" and key is None):
                out[k] = "<T>"
            elif k in ("collectionUUID", "uuid"):
                out[k] = "ID"
            elif k == "txnNumber":
                out[k] = "<N>"
            elif k == "lsid":
                out[k] = "<lsid>"
            else:
                out[k] = clean(v, k, db)
        return out
    if isinstance(value, list):
        if key in ("cursorsKilled", "cursorsNotFound", "cursorsAlive", "cursorsUnknown"):
            return ["CURSOR" if isinstance(v, int) and v > 10**6 else v for v in value]
        return [clean(v, key, db) for v in value]
    if isinstance(value, str):
        text = value.replace(db, "@db") if db else value
        text = re.sub(r"cursor (id )?\d{6,}", r"cursor \1N", text)
        text = UUID_TEXT.sub("UUID", text)
        text = HEX.sub("HEX", text)
        text = re.sub(
            r"expected types '\[([^\]]*)\]'",
            lambda m: "expected types '[" + ", ".join(sorted(m.group(1).split(", "))) + "]'",
            text,
        )
        return re.sub(r"Timestamp\(\d+, \d+\)", "Timestamp(T)", text)
    if isinstance(value, Timestamp):
        return "Timestamp(T)"
    return value


#: mongod keeps the oplog of earlier runs, so a database name is used once.
RUN = uuid.uuid4().hex[:6]


def run_scenario(uri, index, steps):
    db = f"csp{RUN}_{index}"
    wire = Wire(uri)
    state = {"cursors": {}, "events": {}, "pbrt": {}, "salt": uuid.uuid4().hex}
    replies = []
    for step in steps:
        where, command = step if isinstance(step, tuple) else (None, step)
        command = dict(command)
        save = command.pop("save", None)
        where = db if where is None else (db + "_o" if where == "@other" else where)
        try:
            reply = wire.command(where, resolve(command, db, state))
        except TimeoutError:
            wire.close()
            wire = Wire(uri)
            replies.append({"probe": "TIMEOUT"})
            continue
        except (ConnectionError, OSError) as exc:
            wire.close()
            wire = Wire(uri)
            replies.append({"probe": f"CONNECTION {type(exc).__name__}"})
            continue
        if isinstance(reply.get("operationTime"), Timestamp):
            state["optime"] = reply["operationTime"]
        cursor = reply.get("cursor")
        if save and isinstance(cursor, dict):
            state["cursors"][save] = cursor.get("id", 0)
            batch = cursor.get("firstBatch", cursor.get("nextBatch", []))
            state["events"].setdefault(save, []).extend(d for d in batch if isinstance(d, dict))
            if isinstance(cursor.get("postBatchResumeToken"), dict):
                state["pbrt"][save] = cursor["postBatchResumeToken"]
        replies.append(clean(reply, db=db))
    try:
        for name in (db, db + "_o"):
            wire.command(name, {"dropDatabase": 1})
    except (TimeoutError, ConnectionError, OSError):
        print(f"  NOTE: cleanup of {db} did not finish on {uri}", file=sys.stderr)
    finally:
        wire.close()
    return replies


def show(step):
    where, command = step if isinstance(step, tuple) else (None, step)
    body = {k: v for k, v in command.items() if k != "save"}
    return (("" if where is None else f"[{where}] ") + repr(body))[:220]


def main():
    mongod_uri = os.environ.get("PROBE_MONGOD", DEFAULT_MONGOD)
    server_uri = os.environ.get("PROBE_SERVER")
    if not server_uri:
        print("set PROBE_SERVER to the server under test", file=sys.stderr)
        return 2
    only = os.environ.get("PROBE_ONLY")

    # Self-check: the reference must deliver an insert event and a resume
    # token the probe can hand back.
    check = run_scenario(mongod_uri, 0, [watch(), insert({"_id": 1}), more(), watch(resumeAfter=Tok(0), save="r"), insert({"_id": 2}), more("r")])
    try:
        ok = (
            check[2]["cursor"]["nextBatch"][0]["operationType"] == "insert"
            and check[5]["cursor"]["nextBatch"][0]["documentKey"] == {"_id": 2}
        )
    except (KeyError, IndexError, TypeError):
        ok = False
    if not ok:
        print("self-check failed on mongod:", check, file=sys.stderr)
        return 2

    divergent = replies = replies_bad = total = 0
    for index, (name, steps) in enumerate(S, start=1):
        if only and only not in name:
            continue
        total += 1
        want = run_scenario(mongod_uri, index, steps)
        got = run_scenario(server_uri, index, steps)
        replies += len(steps)
        bad = [i for i, (w, g) in enumerate(zip(want, got, strict=True)) if repr(w) != repr(g)]
        if not bad:
            continue
        divergent += 1
        replies_bad += len(bad)
        print(f"\n--- {name}")
        for i in bad:
            print(f"  step {i}: {show(steps[i])}")
            print(f"    mongod: {str(want[i])[:2400]}")
            print(f"    server: {str(got[i])[:2400]}")
    print(
        f"\n=== change streams: {total} scenarios, {replies} replies -- "
        f"{divergent} scenarios and {replies_bad} replies differ ==="
    )
    return 1 if divergent else 0


if __name__ == "__main__":
    sys.exit(main())
