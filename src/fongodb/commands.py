from __future__ import annotations

import datetime as _dt
import os
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

import bson

from fongodb.query import matches
from fongodb.storage import Storage
from fongodb.wire import MAX_BSON_OBJECT_SIZE, MAX_MESSAGE_SIZE

WIRE_VERSION = 17
SERVER_VERSION = "7.0.0"
SERVER_VERSION_ARRAY = [7, 0, 0, 0]


@dataclass
class CommandContext:
    connection_id: int
    storage: Storage
    db_name: str = "admin"


CommandHandler = Callable[[dict[str, Any], CommandContext], dict[str, Any]]


def _hello(_doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    return {
        "isWritablePrimary": True,
        "ismaster": True,
        "topologyVersion": {
            "processId": bson.ObjectId.from_datetime(_dt.datetime.now(_dt.UTC)),
            "counter": 0,
        },
        "maxBsonObjectSize": MAX_BSON_OBJECT_SIZE,
        "maxMessageSizeBytes": MAX_MESSAGE_SIZE,
        "maxWriteBatchSize": 100_000,
        "localTime": _dt.datetime.now(_dt.UTC),
        "logicalSessionTimeoutMinutes": 30,
        "connectionId": ctx.connection_id,
        "minWireVersion": 0,
        "maxWireVersion": WIRE_VERSION,
        "readOnly": False,
        "ok": 1.0,
    }


def _ping(_doc: dict[str, Any], _ctx: CommandContext) -> dict[str, Any]:
    return {"ok": 1.0}


def _build_info(_doc: dict[str, Any], _ctx: CommandContext) -> dict[str, Any]:
    return {
        "version": SERVER_VERSION,
        "gitVersion": "0" * 40,
        "versionArray": SERVER_VERSION_ARRAY,
        "bits": 64,
        "debug": False,
        "maxBsonObjectSize": MAX_BSON_OBJECT_SIZE,
        "ok": 1.0,
    }


def _end_sessions(_doc: dict[str, Any], _ctx: CommandContext) -> dict[str, Any]:
    return {"ok": 1.0}


def _get_log(_doc: dict[str, Any], _ctx: CommandContext) -> dict[str, Any]:
    return {"totalLinesWritten": 0, "log": [], "ok": 1.0}


def _whatsmyuri(_doc: dict[str, Any], _ctx: CommandContext) -> dict[str, Any]:
    return {"you": "127.0.0.1:0", "ok": 1.0}


def _hostinfo(_doc: dict[str, Any], _ctx: CommandContext) -> dict[str, Any]:
    return {
        "system": {
            "currentTime": _dt.datetime.now(_dt.UTC),
            "hostname": "fongodb",
            "cpuAddrSize": 64,
            "memSizeMB": 0,
            "numCores": os.cpu_count() or 1,
            "cpuArch": "x86_64",
            "numaEnabled": False,
        },
        "os": {"type": "fongodb", "name": "fongodb", "version": SERVER_VERSION},
        "ok": 1.0,
    }


def _ns(db: str, coll: str) -> str:
    return f"{db}.{coll}"


def _insert(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    coll = doc["insert"]
    documents = doc.get("documents", [])
    ordered = doc.get("ordered", True)
    inserted, errors = ctx.storage.insert(ctx.db_name, coll, documents, ordered=ordered)
    reply: dict[str, Any] = {"n": inserted, "ok": 1.0}
    if errors:
        reply["writeErrors"] = errors
    return reply


def _find(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    coll = doc["find"]
    filter_ = doc.get("filter") or {}
    skip = int(doc.get("skip", 0) or 0)
    limit = int(doc.get("limit", 0) or 0)
    docs = ctx.storage.find_matching(ctx.db_name, coll, filter_, skip=skip, limit=limit)
    return {
        "cursor": {
            "firstBatch": docs,
            "id": 0,
            "ns": _ns(ctx.db_name, coll),
        },
        "ok": 1.0,
    }


def _update(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    coll = doc["update"]
    updates = doc.get("updates", [])
    n = 0
    n_modified = 0
    upserted: list[dict[str, Any]] = []
    for index, spec in enumerate(updates):
        result = ctx.storage.update_matching(
            ctx.db_name,
            coll,
            spec.get("q", {}),
            spec.get("u", {}),
            multi=bool(spec.get("multi", False)),
            upsert=bool(spec.get("upsert", False)),
        )
        n += result["matched"]
        n_modified += result["modified"]
        if result["upserted_id"] is not None:
            upserted.append({"index": index, "_id": result["upserted_id"]})
            n += 1
    reply: dict[str, Any] = {"n": n, "nModified": n_modified, "ok": 1.0}
    if upserted:
        reply["upserted"] = upserted
    return reply


def _delete(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    coll = doc["delete"]
    deletes = doc.get("deletes", [])
    n = 0
    for spec in deletes:
        n += ctx.storage.delete_matching(
            ctx.db_name, coll, spec.get("q", {}), limit=int(spec.get("limit", 0))
        )
    return {"n": n, "ok": 1.0}


def _count(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    coll = doc["count"]
    filter_ = doc.get("query") or {}
    n = ctx.storage.count_matching(ctx.db_name, coll, filter_)
    return {"n": n, "ok": 1.0}


def _drop(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    coll = doc["drop"]
    existed = ctx.storage.drop_collection(ctx.db_name, coll)
    if not existed:
        return {"ok": 0.0, "errmsg": "ns not found", "code": 26, "codeName": "NamespaceNotFound"}
    return {"ns": _ns(ctx.db_name, coll), "nIndexesWas": 1, "ok": 1.0}


def _drop_database(_doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    ctx.storage.drop_database(ctx.db_name)
    return {"dropped": ctx.db_name, "ok": 1.0}


def _create(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    coll = doc["create"]
    ctx.storage.create_collection(ctx.db_name, coll)
    return {"ok": 1.0}


def _list_collections(_doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    names = ctx.storage.list_collections(ctx.db_name)
    batch = [
        {"name": n, "type": "collection", "options": {}, "info": {"readOnly": False}} for n in names
    ]
    return {
        "cursor": {
            "firstBatch": batch,
            "id": 0,
            "ns": f"{ctx.db_name}.$cmd.listCollections",
        },
        "ok": 1.0,
    }


def _list_databases(_doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    names = ctx.storage.list_databases()
    return {
        "databases": [{"name": n, "sizeOnDisk": 0, "empty": False} for n in names],
        "totalSize": 0,
        "ok": 1.0,
    }


def _list_indexes(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    coll = doc["listIndexes"]
    return {
        "cursor": {
            "firstBatch": [{"v": 2, "key": {"_id": 1}, "name": "_id_"}],
            "id": 0,
            "ns": f"{ctx.db_name}.$cmd.listIndexes.{coll}",
        },
        "ok": 1.0,
    }


def _create_indexes(doc: dict[str, Any], _ctx: CommandContext) -> dict[str, Any]:
    indexes = doc.get("indexes", [])
    return {
        "createdCollectionAutomatically": False,
        "numIndexesBefore": 1,
        "numIndexesAfter": 1 + len(indexes),
        "ok": 1.0,
    }


def _drop_indexes(_doc: dict[str, Any], _ctx: CommandContext) -> dict[str, Any]:
    return {"nIndexesWas": 1, "ok": 1.0}


def _kill_cursors(doc: dict[str, Any], _ctx: CommandContext) -> dict[str, Any]:
    cursor_ids = doc.get("cursors", [])
    return {
        "cursorsKilled": [],
        "cursorsNotFound": list(cursor_ids),
        "cursorsAlive": [],
        "cursorsUnknown": [],
        "ok": 1.0,
    }


def _get_more(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    coll = doc.get("collection", "")
    return {
        "cursor": {"nextBatch": [], "id": 0, "ns": _ns(ctx.db_name, coll)},
        "ok": 1.0,
    }


def _aggregate(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    coll = doc["aggregate"]
    pipeline = doc.get("pipeline", [])
    if isinstance(coll, str):
        docs: list[dict[str, Any]] = ctx.storage.find_matching(ctx.db_name, coll, {})
        ns = _ns(ctx.db_name, coll)
    else:
        docs = []
        ns = f"{ctx.db_name}.$cmd.aggregate"
    for stage in pipeline:
        docs = _apply_stage(stage, docs)
    return {
        "cursor": {"firstBatch": docs, "id": 0, "ns": ns},
        "ok": 1.0,
    }


def _apply_stage(stage: dict[str, Any], docs: list[dict[str, Any]]) -> list[dict[str, Any]]:
    if "$match" in stage:
        return [d for d in docs if matches(d, stage["$match"])]
    if "$count" in stage:
        return [{stage["$count"]: len(docs)}]
    if "$limit" in stage:
        return docs[: int(stage["$limit"])]
    if "$skip" in stage:
        return docs[int(stage["$skip"]) :]
    if "$group" in stage:
        return _stage_group(stage["$group"], docs)
    raise ValueError(f"unsupported aggregation stage: {next(iter(stage))}")


def _stage_group(spec: dict[str, Any], docs: list[dict[str, Any]]) -> list[dict[str, Any]]:
    id_expr = spec.get("_id")
    groups: dict[Any, dict[str, Any]] = {}
    for d in docs:
        key = _eval_group_key(id_expr, d)
        bucket = groups.setdefault(key, {"_id": key})
        for field, accumulator in spec.items():
            if field == "_id":
                continue
            if not isinstance(accumulator, dict):
                continue
            if "$sum" in accumulator:
                addend = accumulator["$sum"]
                value = addend if not isinstance(addend, str) else d.get(addend.lstrip("$"), 0)
                bucket[field] = bucket.get(field, 0) + (value or 0)
    return list(groups.values())


def _eval_group_key(expr: Any, doc: dict[str, Any]) -> Any:
    if isinstance(expr, str) and expr.startswith("$"):
        return doc.get(expr[1:])
    return expr


_HANDLERS: dict[str, CommandHandler] = {
    "hello": _hello,
    "isMaster": _hello,
    "ismaster": _hello,
    "ping": _ping,
    "buildInfo": _build_info,
    "buildinfo": _build_info,
    "endSessions": _end_sessions,
    "getLog": _get_log,
    "whatsmyuri": _whatsmyuri,
    "hostInfo": _hostinfo,
    "insert": _insert,
    "find": _find,
    "update": _update,
    "delete": _delete,
    "count": _count,
    "drop": _drop,
    "create": _create,
    "dropDatabase": _drop_database,
    "listCollections": _list_collections,
    "listDatabases": _list_databases,
    "listIndexes": _list_indexes,
    "createIndexes": _create_indexes,
    "dropIndexes": _drop_indexes,
    "killCursors": _kill_cursors,
    "getMore": _get_more,
    "aggregate": _aggregate,
}


def command_name(doc: dict[str, Any]) -> str:
    return next(iter(doc), "")


def dispatch(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    name = command_name(doc)
    handler = _HANDLERS.get(name)
    if handler is None:
        return {
            "ok": 0.0,
            "errmsg": f"no such command: '{name}'",
            "code": 59,
            "codeName": "CommandNotFound",
        }
    try:
        return handler(doc, ctx)
    except Exception as exc:
        return {
            "ok": 0.0,
            "errmsg": str(exc),
            "code": 14,
            "codeName": "TypeMismatch",
        }
