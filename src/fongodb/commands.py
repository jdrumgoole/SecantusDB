from __future__ import annotations

import datetime as _dt
import os
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

import bson

from fongodb.aggregate import apply_pipeline
from fongodb.cursors import CursorNotFound, CursorRegistry
from fongodb.projection import apply_projection
from fongodb.storage import Storage
from fongodb.wire import MAX_BSON_OBJECT_SIZE, MAX_MESSAGE_SIZE

WIRE_VERSION = 17
SERVER_VERSION = "7.0.0"
SERVER_VERSION_ARRAY = [7, 0, 0, 0]
DEFAULT_BATCH_SIZE = 101


@dataclass
class CommandContext:
    connection_id: int
    storage: Storage
    cursors: CursorRegistry
    db_name: str = "admin"


def _split_into_cursor(
    docs: list[dict[str, Any]],
    batch_size: int,
    namespace: str,
    cursors: CursorRegistry,
) -> tuple[list[dict[str, Any]], int]:
    if batch_size <= 0:
        batch_size = DEFAULT_BATCH_SIZE
    first = docs[:batch_size]
    remaining = docs[batch_size:]
    if not remaining:
        return first, 0
    cursor_id = cursors.register(namespace, remaining)
    return first, cursor_id


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
    sort = doc.get("sort") or None
    projection = doc.get("projection") or None
    batch_size = int(doc.get("batchSize", 0) or 0)
    single_batch = bool(doc.get("singleBatch", False))
    docs = ctx.storage.find_matching(
        ctx.db_name,
        coll,
        filter_,
        skip=skip,
        limit=limit,
        sort=sort,
        projection=projection,
    )
    ns = _ns(ctx.db_name, coll)
    if single_batch:
        first_batch, cursor_id = docs, 0
    else:
        first_batch, cursor_id = _split_into_cursor(
            docs, batch_size or DEFAULT_BATCH_SIZE, ns, ctx.cursors
        )
    return {
        "cursor": {"firstBatch": first_batch, "id": cursor_id, "ns": ns},
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


def _find_and_modify(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    coll = doc["findAndModify"]
    query = doc.get("query") or {}
    sort = doc.get("sort") or None
    fields = doc.get("fields") or None
    return_new = bool(doc.get("new", False))
    upsert = bool(doc.get("upsert", False))
    is_remove = bool(doc.get("remove", False))
    update = doc.get("update")

    if is_remove and update is not None:
        return {
            "ok": 0.0,
            "errmsg": "Cannot specify both update and remove=true",
            "code": 9,
            "codeName": "FailedToParse",
        }
    if not is_remove and update is None:
        return {
            "ok": 0.0,
            "errmsg": "Either an update or remove=true must be specified",
            "code": 9,
            "codeName": "FailedToParse",
        }

    candidates = ctx.storage.find_matching(ctx.db_name, coll, query, sort=sort, limit=1)

    if not candidates:
        if upsert and not is_remove:
            result = ctx.storage.update_matching(
                ctx.db_name, coll, query, update, multi=False, upsert=True
            )
            upserted_id = result["upserted_id"]
            value: Any = None
            if return_new and upserted_id is not None:
                new_docs = ctx.storage.find_matching(ctx.db_name, coll, {"_id": upserted_id})
                if new_docs:
                    value = new_docs[0]
                    if fields:
                        value = apply_projection(value, fields)
            return {
                "lastErrorObject": {
                    "n": 1,
                    "updatedExisting": False,
                    "upserted": upserted_id,
                },
                "value": value,
                "ok": 1.0,
            }
        return {
            "lastErrorObject": {"n": 0, "updatedExisting": False},
            "value": None,
            "ok": 1.0,
        }

    matched_doc = candidates[0]
    matched_id = matched_doc["_id"]

    if is_remove:
        ctx.storage.delete_matching(ctx.db_name, coll, {"_id": matched_id}, limit=1)
        value = matched_doc
        if fields:
            value = apply_projection(value, fields)
        return {
            "lastErrorObject": {"n": 1, "updatedExisting": True},
            "value": value,
            "ok": 1.0,
        }

    ctx.storage.update_matching(ctx.db_name, coll, {"_id": matched_id}, update, multi=False)

    if return_new:
        new_docs = ctx.storage.find_matching(ctx.db_name, coll, {"_id": matched_id})
        value = new_docs[0] if new_docs else None
    else:
        value = matched_doc

    if fields and value is not None:
        value = apply_projection(value, fields)

    return {
        "lastErrorObject": {"n": 1, "updatedExisting": True},
        "value": value,
        "ok": 1.0,
    }


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


def _kill_cursors(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    cursor_ids = [int(c) for c in doc.get("cursors", [])]
    killed, not_found = ctx.cursors.kill(cursor_ids)
    return {
        "cursorsKilled": killed,
        "cursorsNotFound": not_found,
        "cursorsAlive": [],
        "cursorsUnknown": [],
        "ok": 1.0,
    }


def _get_more(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    cursor_id = int(doc["getMore"])
    coll = doc.get("collection", "")
    batch_size = int(doc.get("batchSize", 0) or 0)
    ns = _ns(ctx.db_name, coll)
    try:
        batch, exhausted = ctx.cursors.next_batch(cursor_id, batch_size or DEFAULT_BATCH_SIZE)
    except CursorNotFound:
        return {
            "ok": 0.0,
            "errmsg": f"cursor id {cursor_id} not found",
            "code": 43,
            "codeName": "CursorNotFound",
        }
    return {
        "cursor": {
            "nextBatch": batch,
            "id": 0 if exhausted else cursor_id,
            "ns": ns,
        },
        "ok": 1.0,
    }


def _aggregate(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    coll = doc["aggregate"]
    pipeline = doc.get("pipeline", [])
    cursor_opts = doc.get("cursor") or {}
    batch_size = int(cursor_opts.get("batchSize", 0) or 0)
    if isinstance(coll, str):
        docs: list[dict[str, Any]] = ctx.storage.find_matching(ctx.db_name, coll, {})
        ns = _ns(ctx.db_name, coll)
    else:
        docs = []
        ns = f"{ctx.db_name}.$cmd.aggregate"
    docs = apply_pipeline(docs, pipeline)
    first_batch, cursor_id = _split_into_cursor(
        docs, batch_size or DEFAULT_BATCH_SIZE, ns, ctx.cursors
    )
    return {
        "cursor": {"firstBatch": first_batch, "id": cursor_id, "ns": ns},
        "ok": 1.0,
    }


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
    "findAndModify": _find_and_modify,
    "findandmodify": _find_and_modify,
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
