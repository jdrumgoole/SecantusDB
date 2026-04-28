from __future__ import annotations

import datetime as _dt
import os
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

import bson

from secantus.aggregate import PipelineContext, apply_pipeline
from secantus.cursors import CursorNotFound, CursorRegistry
from secantus.projection import apply_projection
from secantus.storage import Storage
from secantus.wire import MAX_BSON_OBJECT_SIZE, MAX_MESSAGE_SIZE

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


def _start_session(_doc: dict[str, Any], _ctx: CommandContext) -> dict[str, Any]:
    import uuid

    from bson import Binary

    return {
        "id": {"id": Binary(uuid.uuid4().bytes, 4)},
        "timeoutMinutes": 30,
        "ok": 1.0,
    }


def _refresh_sessions(_doc: dict[str, Any], _ctx: CommandContext) -> dict[str, Any]:
    return {"ok": 1.0}


def _abort_transaction(_doc: dict[str, Any], _ctx: CommandContext) -> dict[str, Any]:
    return {"ok": 1.0}


def _commit_transaction(_doc: dict[str, Any], _ctx: CommandContext) -> dict[str, Any]:
    return {"ok": 1.0}


def _get_log(_doc: dict[str, Any], _ctx: CommandContext) -> dict[str, Any]:
    return {"totalLinesWritten": 0, "log": [], "ok": 1.0}


def _whatsmyuri(_doc: dict[str, Any], _ctx: CommandContext) -> dict[str, Any]:
    return {"you": "127.0.0.1:0", "ok": 1.0}


def _hostinfo(_doc: dict[str, Any], _ctx: CommandContext) -> dict[str, Any]:
    return {
        "system": {
            "currentTime": _dt.datetime.now(_dt.UTC),
            "hostname": "secantus",
            "cpuAddrSize": 64,
            "memSizeMB": 0,
            "numCores": os.cpu_count() or 1,
            "cpuArch": "x86_64",
            "numaEnabled": False,
        },
        "os": {"type": "secantus", "name": "secantus", "version": SERVER_VERSION},
        "ok": 1.0,
    }


def _server_status(_doc: dict[str, Any], _ctx: CommandContext) -> dict[str, Any]:
    return {
        "host": "secantus",
        "version": SERVER_VERSION,
        "process": "secantus",
        "pid": os.getpid(),
        "uptime": 0,
        "uptimeMillis": 0,
        "uptimeEstimate": 0,
        "localTime": _dt.datetime.now(_dt.UTC),
        "ok": 1.0,
    }


def _connection_status(_doc: dict[str, Any], _ctx: CommandContext) -> dict[str, Any]:
    return {
        "authInfo": {
            "authenticatedUsers": [],
            "authenticatedUserRoles": [],
            "authenticatedUserPrivileges": [],
        },
        "ok": 1.0,
    }


def _db_stats(_doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    colls = ctx.storage.list_collections(ctx.db_name)
    objects = sum(ctx.storage.count_matching(ctx.db_name, c, None) for c in colls)
    return {
        "db": ctx.db_name,
        "collections": len(colls),
        "objects": objects,
        "avgObjSize": 0,
        "dataSize": 0,
        "storageSize": 0,
        "indexes": sum(len(ctx.storage.list_indexes(ctx.db_name, c)) for c in colls),
        "indexSize": 0,
        "totalSize": 0,
        "scaleFactor": 1,
        "ok": 1.0,
    }


def _coll_stats(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    coll = doc.get("collStats")
    if not isinstance(coll, str):
        return {
            "ok": 0.0,
            "errmsg": "collStats requires a collection name",
            "code": 14,
            "codeName": "TypeMismatch",
        }
    if not ctx.storage.collection_exists(ctx.db_name, coll):
        return {
            "ok": 0.0,
            "errmsg": f"ns not found: {ctx.db_name}.{coll}",
            "code": 26,
            "codeName": "NamespaceNotFound",
        }
    return {
        "ns": f"{ctx.db_name}.{coll}",
        "count": ctx.storage.count_matching(ctx.db_name, coll, None),
        "size": 0,
        "avgObjSize": 0,
        "storageSize": 0,
        "totalIndexSize": 0,
        "indexSizes": {},
        "nindexes": len(ctx.storage.list_indexes(ctx.db_name, coll)),
        "scaleFactor": 1,
        "capped": False,
        "ok": 1.0,
    }


def _explain(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    inner = doc.get("explain") or {}
    if isinstance(inner, dict):
        cmd_name = next(iter(inner), "")
        coll_value = inner.get(cmd_name)
        coll = coll_value if isinstance(coll_value, str) else ""
        filter_ = inner.get("filter") or inner.get("query") or {}
    else:
        coll = ""
        filter_ = {}
    namespace = _ns(ctx.db_name, coll) if coll else f"{ctx.db_name}.$cmd"
    return {
        "queryPlanner": {
            "namespace": namespace,
            "indexFilterSet": False,
            "parsedQuery": filter_,
            "winningPlan": {"stage": "COLLSCAN", "filter": filter_},
            "rejectedPlans": [],
        },
        "executionStats": {
            "executionSuccess": True,
            "nReturned": 0,
            "executionTimeMillis": 0,
            "totalKeysExamined": 0,
            "totalDocsExamined": 0,
            "executionStages": {"stage": "COLLSCAN", "nReturned": 0},
        },
        "command": inner if isinstance(inner, dict) else {},
        "serverInfo": {
            "host": "secantus",
            "port": 0,
            "version": SERVER_VERSION,
            "gitVersion": "0" * 40,
        },
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
    from secantus.storage import IndexConflict

    coll = doc["update"]
    updates = doc.get("updates", [])
    ordered = bool(doc.get("ordered", True))
    n = 0
    n_modified = 0
    upserted: list[dict[str, Any]] = []
    write_errors: list[dict[str, Any]] = []
    for index, spec in enumerate(updates):
        try:
            result = ctx.storage.update_matching(
                ctx.db_name,
                coll,
                spec.get("q", {}),
                spec.get("u", {}),
                multi=bool(spec.get("multi", False)),
                upsert=bool(spec.get("upsert", False)),
                array_filters=spec.get("arrayFilters"),
            )
        except IndexConflict as exc:
            write_errors.append({"index": index, "code": 11000, "errmsg": str(exc)})
            if ordered:
                break
            continue
        n += result["matched"]
        n_modified += result["modified"]
        if result["upserted_id"] is not None:
            upserted.append({"index": index, "_id": result["upserted_id"]})
            n += 1
    reply: dict[str, Any] = {"n": n, "nModified": n_modified, "ok": 1.0}
    if upserted:
        reply["upserted"] = upserted
    if write_errors:
        reply["writeErrors"] = write_errors
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


def _distinct(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    from secantus.paths import get_path

    coll = doc["distinct"]
    key = doc.get("key", "")
    filter_ = doc.get("query") or {}
    if not isinstance(key, str):
        return {
            "ok": 0.0,
            "errmsg": "distinct key must be a string",
            "code": 14,
            "codeName": "TypeMismatch",
        }
    matched = ctx.storage.find_matching(ctx.db_name, coll, filter_)
    seen: list[Any] = []
    for d in matched:
        value = get_path(d, key)
        if isinstance(value, list):
            for elem in value:
                if elem not in seen:
                    seen.append(elem)
        elif (value is not None or _key_present(d, key)) and value not in seen:
            seen.append(value)
    return {"values": seen, "ok": 1.0}


def _key_present(doc: dict[str, Any], path: str) -> bool:
    cur: Any = doc
    for part in path.split("."):
        if isinstance(cur, dict) and part in cur:
            cur = cur[part]
        else:
            return False
    return True


def _find_and_modify(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    from secantus.storage import IndexConflict

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
            try:
                result = ctx.storage.update_matching(
                    ctx.db_name, coll, query, update, multi=False, upsert=True
                )
            except IndexConflict as exc:
                return {
                    "ok": 0.0,
                    "errmsg": str(exc),
                    "code": 11000,
                    "codeName": "DuplicateKey",
                }
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

    try:
        ctx.storage.update_matching(ctx.db_name, coll, {"_id": matched_id}, update, multi=False)
    except IndexConflict as exc:
        return {
            "ok": 0.0,
            "errmsg": str(exc),
            "code": 11000,
            "codeName": "DuplicateKey",
        }

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


def _rename_collection(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    src_ns = doc.get("renameCollection")
    dst_ns = doc.get("to")
    drop_target = bool(doc.get("dropTarget", False))
    if not isinstance(src_ns, str) or not isinstance(dst_ns, str):
        return {
            "ok": 0.0,
            "errmsg": "renameCollection requires two string namespaces",
            "code": 14,
            "codeName": "TypeMismatch",
        }
    src_db, _, src_coll = src_ns.partition(".")
    dst_db, _, dst_coll = dst_ns.partition(".")
    if not src_coll or not dst_coll:
        return {
            "ok": 0.0,
            "errmsg": "renameCollection namespaces must be of the form db.coll",
            "code": 73,
            "codeName": "InvalidNamespace",
        }
    ok, err = ctx.storage.rename_collection(
        src_db, src_coll, dst_db, dst_coll, drop_target=drop_target
    )
    if not ok:
        code = 26 if err and "does not exist" in err else 48
        code_name = "NamespaceNotFound" if code == 26 else "NamespaceExists"
        return {"ok": 0.0, "errmsg": err, "code": code, "codeName": code_name}
    return {"ok": 1.0}


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
    indexes = ctx.storage.list_indexes(ctx.db_name, coll)
    if not indexes:
        return {
            "ok": 0.0,
            "errmsg": f"ns does not exist: {ctx.db_name}.{coll}",
            "code": 26,
            "codeName": "NamespaceNotFound",
        }
    return {
        "cursor": {
            "firstBatch": indexes,
            "id": 0,
            "ns": f"{ctx.db_name}.$cmd.listIndexes.{coll}",
        },
        "ok": 1.0,
    }


def _create_indexes(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    from secantus.storage import IndexConflict

    coll = doc["createIndexes"]
    indexes = doc.get("indexes", [])
    created_auto = not ctx.storage.collection_exists(ctx.db_name, coll)
    num_before = len(ctx.storage.list_indexes(ctx.db_name, coll)) if not created_auto else 1
    ctx.storage.create_collection(ctx.db_name, coll)
    created = 0
    for idx_spec in indexes:
        if not isinstance(idx_spec, dict):
            return {
                "ok": 0.0,
                "errmsg": "index spec must be a document",
                "code": 14,
                "codeName": "TypeMismatch",
            }
        key_spec = idx_spec.get("key")
        name = idx_spec.get("name")
        if not isinstance(key_spec, dict) or not isinstance(name, str):
            return {
                "ok": 0.0,
                "errmsg": "index requires key (document) and name (string)",
                "code": 14,
                "codeName": "TypeMismatch",
            }
        options = {k: v for k, v in idx_spec.items() if k not in ("key", "name")}
        try:
            new = ctx.storage.create_index(ctx.db_name, coll, name, key_spec, options)
        except IndexConflict as exc:
            return {
                "ok": 0.0,
                "errmsg": str(exc),
                "code": 11000,
                "codeName": "DuplicateKey",
            }
        if new:
            created += 1
    return {
        "createdCollectionAutomatically": created_auto,
        "numIndexesBefore": num_before,
        "numIndexesAfter": num_before + created,
        "ok": 1.0,
    }


def _drop_indexes(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    coll = doc["dropIndexes"]
    target = doc.get("index", "*")
    num_before = len(ctx.storage.list_indexes(ctx.db_name, coll))
    if num_before == 0:
        return {
            "ok": 0.0,
            "errmsg": f"ns not found: {ctx.db_name}.{coll}",
            "code": 26,
            "codeName": "NamespaceNotFound",
        }
    if target == "*":
        ctx.storage.drop_all_indexes(ctx.db_name, coll)
        return {"nIndexesWas": num_before, "ok": 1.0}
    if target == "_id_":
        return {
            "ok": 0.0,
            "errmsg": "cannot drop _id index",
            "code": 67,
            "codeName": "InvalidOptions",
        }
    if not isinstance(target, str):
        return {
            "ok": 0.0,
            "errmsg": "index must be a string name or '*'",
            "code": 14,
            "codeName": "TypeMismatch",
        }
    ok = ctx.storage.drop_index(ctx.db_name, coll, target)
    if not ok:
        return {
            "ok": 0.0,
            "errmsg": f"index not found with name [{target}]",
            "code": 27,
            "codeName": "IndexNotFound",
        }
    return {"nIndexesWas": num_before, "ok": 1.0}


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
    docs = apply_pipeline(docs, pipeline, PipelineContext(storage=ctx.storage, db_name=ctx.db_name))
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
    "startSession": _start_session,
    "refreshSessions": _refresh_sessions,
    "abortTransaction": _abort_transaction,
    "commitTransaction": _commit_transaction,
    "getLog": _get_log,
    "whatsmyuri": _whatsmyuri,
    "hostInfo": _hostinfo,
    "explain": _explain,
    "serverStatus": _server_status,
    "connectionStatus": _connection_status,
    "dbStats": _db_stats,
    "collStats": _coll_stats,
    "insert": _insert,
    "find": _find,
    "update": _update,
    "delete": _delete,
    "count": _count,
    "distinct": _distinct,
    "findAndModify": _find_and_modify,
    "findandmodify": _find_and_modify,
    "drop": _drop,
    "create": _create,
    "dropDatabase": _drop_database,
    "renameCollection": _rename_collection,
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
