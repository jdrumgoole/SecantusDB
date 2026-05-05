from __future__ import annotations

import datetime as _dt
import os
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from typing import Any

import bson

from secantus.aggregate import PipelineContext, apply_pipeline
from secantus.auth import (
    SCRAM_SHA_256,
    AuthError,
    ConnectionAuth,
    StoredCredentials,
    begin_scram,
    continue_scram,
    derive_credentials,
)
from secantus.cursors import CursorNotFound, CursorRegistry
from secantus.projection import apply_projection
from secantus.rbac import (
    A_CHANGE_PASSWORD,
    A_COLL_MOD,
    A_COLL_STATS,
    A_CREATE_COLLECTION,
    A_CREATE_INDEX,
    A_CREATE_USER,
    A_DB_STATS,
    A_DROP_COLLECTION,
    A_DROP_DATABASE,
    A_DROP_INDEX,
    A_DROP_USER,
    A_FIND,
    A_GET_CMD_LINE_OPTS,
    A_GET_LOG,
    A_GRANT_ROLE,
    A_HOST_INFO,
    A_INSERT,
    A_KILL_CURSORS,
    A_LIST_COLLECTIONS,
    A_LIST_DATABASES,
    A_LIST_INDEXES,
    A_REMOVE,
    A_RENAME_COLL_SAME_DB,
    A_REVOKE_ROLE,
    A_SERVER_STATUS,
    A_UPDATE,
    A_VIEW_ROLE,
    A_VIEW_USER,
    BUILT_IN_ROLES,
    SCOPE_CLUSTER,
    SCOPE_COLLECTION,
    SCOPE_DATABASE,
    check_privilege,
    is_known_role,
)
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
    server_address: tuple[str, int] | None = None
    replica_set_name: str | None = None
    connection_auth: ConnectionAuth | None = None
    require_auth: bool = False


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


def _hello(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    # `topologyVersion.counter` and `connectionId` MUST be int64 on the wire.
    # pymongo accepts either, but the official Go driver (mongodump/restore,
    # mongo-go-driver) refuses the handshake with "expected 'counter' to be
    # an int64 but it's a BSON 32-bit integer" when these are int32.
    response: dict[str, Any] = {
        "isWritablePrimary": True,
        "ismaster": True,
        "topologyVersion": {
            "processId": bson.ObjectId.from_datetime(_dt.datetime.now(_dt.timezone.utc)),
            "counter": bson.Int64(0),
        },
        "maxBsonObjectSize": MAX_BSON_OBJECT_SIZE,
        "maxMessageSizeBytes": MAX_MESSAGE_SIZE,
        "maxWriteBatchSize": 100_000,
        "localTime": _dt.datetime.now(_dt.timezone.utc),
        "logicalSessionTimeoutMinutes": 30,
        "connectionId": bson.Int64(ctx.connection_id),
        "minWireVersion": 0,
        "maxWireVersion": WIRE_VERSION,
        "readOnly": False,
        "ok": 1.0,
    }
    if ctx.replica_set_name and ctx.server_address is not None:
        addr = f"{ctx.server_address[0]}:{ctx.server_address[1]}"
        cluster_time = ctx.storage.current_cluster_time()
        response.update(
            {
                "setName": ctx.replica_set_name,
                "setVersion": 1,
                "hosts": [addr],
                "passives": [],
                "arbiters": [],
                "primary": addr,
                "me": addr,
                "electionId": bson.ObjectId("7fffffff0000000000000001"),
                "lastWrite": {
                    "opTime": {"ts": cluster_time, "t": 1},
                    "lastWriteDate": _dt.datetime.now(_dt.timezone.utc),
                    "majorityOpTime": {"ts": cluster_time, "t": 1},
                    "majorityWriteDate": _dt.datetime.now(_dt.timezone.utc),
                },
            }
        )
    # saslSupportedMechs: drivers pass `saslSupportedMechs: "<db>.<user>"`
    # to discover which mechanisms they should attempt for that principal.
    # We always advertise SCRAM-SHA-256 (the modern MongoDB default); older
    # SCRAM-SHA-1 is intentionally not offered.
    sasl_mechs_for = doc.get("saslSupportedMechs")
    if isinstance(sasl_mechs_for, str):
        response["saslSupportedMechs"] = [SCRAM_SHA_256]
    if ctx.require_auth:
        # Tell the client this server has access control on so it knows
        # to treat the connection as needing auth before commands flow.
        response["accessControlEnabled"] = True
    return response


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
            "currentTime": _dt.datetime.now(_dt.timezone.utc),
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
        "localTime": _dt.datetime.now(_dt.timezone.utc),
        "ok": 1.0,
    }


def _connection_status(_doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    principals = ctx.connection_auth.authenticated_principals if ctx.connection_auth else []
    return {
        "authInfo": {
            "authenticatedUsers": [{"user": user, "db": db} for db, user in principals],
            # RBAC isn't implemented; an authenticated user is treated as
            # fully privileged. Surface an empty role/privilege list — most
            # clients only check authenticatedUsers.
            "authenticatedUserRoles": [],
            "authenticatedUserPrivileges": [],
        },
        "ok": 1.0,
    }


def _db_stats(_doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    colls = ctx.storage.list_collections(ctx.db_name)
    objects = sum(ctx.storage.count_matching(ctx.db_name, c, None) for c in colls)
    data_size = sum(ctx.storage.collection_data_size(ctx.db_name, c) for c in colls)
    index_size = sum(sum(ctx.storage.index_sizes(ctx.db_name, c).values()) for c in colls)
    avg_obj_size = (data_size / objects) if objects else 0
    return {
        "db": ctx.db_name,
        "collections": len(colls),
        "objects": objects,
        "avgObjSize": avg_obj_size,
        "dataSize": data_size,
        "storageSize": data_size,
        "indexes": sum(len(ctx.storage.list_indexes(ctx.db_name, c)) for c in colls),
        "indexSize": index_size,
        "totalSize": data_size + index_size,
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
    count = ctx.storage.count_matching(ctx.db_name, coll, None)
    data_size = ctx.storage.collection_data_size(ctx.db_name, coll)
    index_sizes = ctx.storage.index_sizes(ctx.db_name, coll)
    total_index_size = sum(index_sizes.values())
    avg_obj_size = (data_size / count) if count else 0
    return {
        "ns": f"{ctx.db_name}.{coll}",
        "count": count,
        "size": data_size,
        "avgObjSize": avg_obj_size,
        "storageSize": data_size,
        "totalIndexSize": total_index_size,
        "indexSizes": index_sizes,
        "nindexes": len(ctx.storage.list_indexes(ctx.db_name, coll)),
        "scaleFactor": 1,
        "capped": False,
        "ok": 1.0,
    }


def _explain(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    inner = doc.get("explain") or {}
    coll = ""
    filter_: dict[str, Any] = {}
    sort = None
    hint = None
    if isinstance(inner, dict):
        cmd_name = next(iter(inner), "")
        coll_value = inner.get(cmd_name)
        coll = coll_value if isinstance(coll_value, str) else ""
        filter_ = inner.get("filter") or inner.get("query") or {}
        sort = inner.get("sort")
        hint = inner.get("hint")
    namespace = _ns(ctx.db_name, coll) if coll else f"{ctx.db_name}.$cmd"
    if coll:
        plan = ctx.storage.explain_plan(ctx.db_name, coll, filter_, sort=sort, hint=hint)
    else:
        plan = {"kind": "COLLSCAN"}
    if plan["kind"] == "IXSCAN":
        winning_plan = {
            "stage": "FETCH",
            "filter": filter_,
            "inputStage": {
                "stage": "IXSCAN",
                "indexName": plan["index_name"],
                "keyPattern": plan["key_pattern"],
                "direction": plan["direction"],
            },
        }
        execution_stage = {
            "stage": "FETCH",
            "nReturned": 0,
            "inputStage": {"stage": "IXSCAN", "nReturned": 0},
        }
    else:
        winning_plan = {"stage": "COLLSCAN", "filter": filter_}
        execution_stage = {"stage": "COLLSCAN", "nReturned": 0}
    return {
        "queryPlanner": {
            "namespace": namespace,
            "indexFilterSet": False,
            "parsedQuery": filter_,
            "winningPlan": winning_plan,
            "rejectedPlans": [],
        },
        "executionStats": {
            "executionSuccess": True,
            "nReturned": 0,
            "executionTimeMillis": 0,
            "totalKeysExamined": 0,
            "totalDocsExamined": 0,
            "executionStages": execution_stage,
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
    from secantus.storage import BadHint

    coll = doc["find"]
    filter_ = doc.get("filter") or {}
    skip = int(doc.get("skip", 0) or 0)
    limit = int(doc.get("limit", 0) or 0)
    sort = doc.get("sort") or None
    projection = doc.get("projection") or None
    hint = doc.get("hint")
    batch_size = int(doc.get("batchSize", 0) or 0)
    single_batch = bool(doc.get("singleBatch", False))
    try:
        docs = ctx.storage.find_matching(
            ctx.db_name,
            coll,
            filter_,
            skip=skip,
            limit=limit,
            sort=sort,
            projection=projection,
            hint=hint,
        )
    except BadHint as exc:
        return {"ok": 0.0, "errmsg": str(exc), "code": 2, "codeName": "BadValue"}
    ns = _ns(ctx.db_name, coll)
    if single_batch:
        first_batch, cursor_id = docs, 0
    else:
        first_batch, cursor_id = _split_into_cursor(
            docs, batch_size or DEFAULT_BATCH_SIZE, ns, ctx.cursors
        )
    return {
        # Cursor `id` MUST be int64 — the Go driver hard-fails int32 here.
        "cursor": {"firstBatch": first_batch, "id": bson.Int64(cursor_id), "ns": ns},
        "ok": 1.0,
    }


def _update(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    from secantus.storage import GeoExtractError, IndexConflict

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
        except GeoExtractError as exc:
            # Mongod's documented code for "Can't extract geo keys from
            # object" — surfaces to the driver as a write error on the
            # specific operation index without aborting unordered batches.
            write_errors.append({"index": index, "code": 16572, "errmsg": str(exc)})
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
    from secantus.storage import GeoExtractError, IndexConflict

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
            except GeoExtractError as exc:
                return {
                    "ok": 0.0,
                    "errmsg": str(exc),
                    "code": 16572,
                    "codeName": "Location16572",
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
    except GeoExtractError as exc:
        return {
            "ok": 0.0,
            "errmsg": str(exc),
            "code": 16572,
            "codeName": "Location16572",
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
    pre_post = doc.get("changeStreamPreAndPostImages")
    if isinstance(pre_post, Mapping):
        ctx.storage.set_collection_options(
            ctx.db_name, coll, changeStreamPreAndPostImages=dict(pre_post)
        )
    return {"ok": 1.0}


def _coll_mod(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    coll = doc["collMod"]
    if not ctx.storage.collection_exists(ctx.db_name, coll):
        return {
            "ok": 0.0,
            "errmsg": f"ns not found: {ctx.db_name}.{coll}",
            "code": 26,
            "codeName": "NamespaceNotFound",
        }
    pre_post = doc.get("changeStreamPreAndPostImages")
    if isinstance(pre_post, Mapping):
        ctx.storage.set_collection_options(
            ctx.db_name, coll, changeStreamPreAndPostImages=dict(pre_post)
        )
    return {"ok": 1.0}


def _list_collections(_doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    names = ctx.storage.list_collections(ctx.db_name)
    batch = [
        {"name": n, "type": "collection", "options": {}, "info": {"readOnly": False}} for n in names
    ]
    return {
        "cursor": {
            "firstBatch": batch,
            "id": bson.Int64(0),
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
            "id": bson.Int64(0),
            "ns": f"{ctx.db_name}.$cmd.listIndexes.{coll}",
        },
        "ok": 1.0,
    }


def _create_indexes(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    from secantus.storage import GeoExtractError, IndexConflict

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
        except GeoExtractError as exc:
            # `createIndex` on existing docs hit a doc the geo extractor
            # can't make sense of — fail the whole index creation, like
            # mongod. The collection is left without the index; the
            # client must clean up bad docs before retrying.
            return {
                "ok": 0.0,
                "errmsg": str(exc),
                "code": 16572,
                "codeName": "Location16572",
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
    # Wake any in-flight `_get_more` on these cursors BEFORE removing them
    # from the registry. The tailable getMore handler holds an `entry`
    # reference fetched at command start and sleeps in
    # `_oplog_cv.wait_for(...)` for up to its maxTimeMS / 1s budget. If we
    # only `pop` from the registry without touching `entry.invalidated` or
    # notifying the cv, that thread sleeps the full budget, wakes, sees
    # `entry.invalidated == False`, and returns `cursor.id != 0` — telling
    # the client the cursor is still alive after the client just killed it.
    # Drivers like the official Java driver then block in
    # `waitForLastRelease(...)` waiting for the connection holding that
    # zombie getMore to go idle, which manifests as the validate-java
    # suite hanging on `ChangeStreamOperationProseTestSpecification`.
    for cid in cursor_ids:
        try:
            entry = ctx.cursors.get(cid)
        except CursorNotFound:
            continue
        entry.invalidated = True
    if cursor_ids:
        with ctx.storage._oplog_cv:
            ctx.storage._oplog_cv.notify_all()
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
    batch_size = int(doc.get("batchSize", 0) or 0) or DEFAULT_BATCH_SIZE
    max_time_ms = int(doc.get("maxTimeMS", 0) or 0)
    ns = _ns(ctx.db_name, coll)
    try:
        entry = ctx.cursors.get(cursor_id)
    except CursorNotFound:
        return {
            "ok": 0.0,
            "errmsg": f"cursor id {cursor_id} not found",
            "code": 43,
            "codeName": "CursorNotFound",
        }
    if not entry.tailable:
        try:
            batch, exhausted = ctx.cursors.next_batch(cursor_id, batch_size)
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
                "id": bson.Int64(0 if exhausted else cursor_id),
                "ns": ns,
            },
            "ok": 1.0,
        }
    # Tailable path.
    if entry.invalidated and not entry.final_event_pending and not entry.remaining:
        ctx.cursors.kill([cursor_id])
        return {
            "cursor": {"nextBatch": [], "id": bson.Int64(0), "ns": ns},
            "ok": 1.0,
        }
    # Drain any already-buffered events first.
    if not entry.remaining and entry.producer is not None:
        new_events = entry.producer()
        if new_events:
            entry.remaining.extend(new_events)
    if not entry.remaining and entry.await_data and not entry.invalidated:
        # PyMongo does not always pass maxTimeMS on getMore for change streams;
        # real mongod treats that as "wait indefinitely". We bound the wait so
        # the connection thread can be reaped on shutdown.
        wait_seconds = max_time_ms / 1000.0 if max_time_ms > 0 else 1.0
        captured_tail = ctx.storage.oplog_tail_seq()
        with ctx.storage._oplog_cv:
            ctx.storage._oplog_cv.wait_for(
                lambda: ctx.storage.oplog_tail_seq() > captured_tail or entry.invalidated,
                timeout=wait_seconds,
            )
        if entry.producer is not None and not entry.remaining:
            new_events = entry.producer()
            if new_events:
                entry.remaining.extend(new_events)
    batch = entry.remaining[:batch_size]
    entry.remaining = entry.remaining[batch_size:]
    if not entry.remaining and entry.invalidated and entry.final_event_pending:
        # The invalidate event has now been delivered.
        entry.final_event_pending = False
    cursor_alive = not (entry.invalidated and not entry.remaining and not entry.final_event_pending)
    return {
        "cursor": {
            "nextBatch": batch,
            # Cursor `id` MUST be int64 — Go driver hard-fails int32.
            "id": bson.Int64(cursor_id if cursor_alive else 0),
            "ns": ns,
        },
        "ok": 1.0,
    }


def _aggregate(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    from secantus import changestreams
    from secantus.storage import BadHint

    coll = doc["aggregate"]
    pipeline = doc.get("pipeline", [])
    hint = doc.get("hint")
    cursor_opts = doc.get("cursor") or {}
    batch_size = int(cursor_opts.get("batchSize", 0) or 0)
    coll_name = ""

    first_stage = pipeline[0] if pipeline else {}
    is_change_stream = isinstance(first_stage, Mapping) and "$changeStream" in first_stage

    if is_change_stream:
        return _aggregate_change_stream(doc, ctx, coll, pipeline, batch_size)

    if isinstance(coll, str):
        coll_name = coll
        if isinstance(first_stage, Mapping) and (
            "$collStats" in first_stage
            or "$indexStats" in first_stage
            or "$documents" in first_stage
        ):
            docs: list[dict[str, Any]] = []
        else:
            # If the first stage is $match, lift its filter into the initial
            # fetch so the index planner can use it. Skip the stage in the
            # pipeline so we don't apply the same filter twice.
            initial_filter: dict[str, Any] = {}
            if (
                isinstance(first_stage, Mapping)
                and "$match" in first_stage
                and isinstance(first_stage["$match"], Mapping)
            ):
                initial_filter = dict(first_stage["$match"])
                pipeline = list(pipeline[1:])
            try:
                docs = ctx.storage.find_matching(ctx.db_name, coll, initial_filter, hint=hint)
            except BadHint as exc:
                return {"ok": 0.0, "errmsg": str(exc), "code": 2, "codeName": "BadValue"}
        ns = _ns(ctx.db_name, coll)
    else:
        docs = []
        ns = f"{ctx.db_name}.$cmd.aggregate"
    pipeline_ctx = PipelineContext(storage=ctx.storage, db_name=ctx.db_name, coll_name=coll_name)
    docs = apply_pipeline(docs, pipeline, pipeline_ctx)
    first_batch, cursor_id = _split_into_cursor(
        docs, batch_size or DEFAULT_BATCH_SIZE, ns, ctx.cursors
    )
    # Silence unused import in this branch.
    _ = changestreams
    return {
        # Cursor `id` MUST be int64 — the Go driver hard-fails int32 here.
        "cursor": {"firstBatch": first_batch, "id": bson.Int64(cursor_id), "ns": ns},
        "ok": 1.0,
    }


def _aggregate_change_stream(
    doc: dict[str, Any],
    ctx: CommandContext,
    coll: Any,
    pipeline: list[dict[str, Any]],
    batch_size: int,
) -> dict[str, Any]:
    from secantus import changestreams

    storage = ctx.storage
    spec = pipeline[0]["$changeStream"]
    if not isinstance(spec, Mapping):
        return {
            "ok": 0.0,
            "errmsg": "$changeStream spec must be a document",
            "code": 2,
            "codeName": "BadValue",
        }
    cs_spec = changestreams.parse_spec(spec)

    # Determine scope.
    if isinstance(coll, str):
        scope = {"kind": "coll", "db": ctx.db_name, "coll": coll}
        ns = _ns(ctx.db_name, coll)
        coll_name = coll
        coll_uuid = None
        if storage.collection_exists(ctx.db_name, coll):
            coll_uuid = storage.collection_uuid(ctx.db_name, coll)
    else:
        if ctx.db_name == "admin":
            scope = {"kind": "cluster"}
            ns = f"{ctx.db_name}.$cmd.aggregate"
        else:
            scope = {"kind": "db", "db": ctx.db_name}
            ns = f"{ctx.db_name}.$cmd.aggregate"
        coll_name = ""
        coll_uuid = None

    # Resolve start position.
    floor = storage.oplog_floor_seq()
    tail = storage.oplog_tail_seq()
    if cs_spec.resume_after is not None or cs_spec.start_after is not None:
        token = cs_spec.resume_after or cs_spec.start_after
        try:
            data = changestreams.parse_resume_token(token)
        except (ValueError, KeyError) as exc:
            return {
                "ok": 0.0,
                "errmsg": f"invalid resume token: {exc}",
                "code": 9,
                "codeName": "FailedToParse",
            }
        history_lost = False
        if floor > 0 and data.seq < floor:
            history_lost = True
        # Empty oplog after pruning: any token referencing a past-emitted seq
        # is unresumable.
        if floor == 0 and tail > 0 and data.seq <= tail:
            history_lost = True
        if history_lost:
            return {
                "ok": 0.0,
                "errmsg": (
                    "Resume of change stream was not possible, as the resume "
                    "point may no longer be in the oplog."
                ),
                "code": changestreams.ChangeStreamHistoryLost.code,
                "codeName": changestreams.ChangeStreamHistoryLost.codeName,
            }
        start_seq = data.seq + 1
    elif cs_spec.start_at_operation_time is not None:
        start_seq = storage.find_seq_for_ts(cs_spec.start_at_operation_time)
    else:
        start_seq = storage.oplog_tail_seq() + 1

    # Build the namespace filter once for cheap reuse.
    def _ns_filter(entry_ns: str) -> bool:
        kind = scope.get("kind")
        if kind == "cluster":
            return True
        if kind == "db":
            db_target = scope.get("db")
            return entry_ns.startswith(f"{db_target}.")
        if kind == "coll":
            db_target = scope.get("db")
            coll_target = scope.get("coll")
            return entry_ns == f"{db_target}.{coll_target}" or entry_ns == f"{db_target}.$cmd"
        return False

    pipeline_after_cs = list(pipeline[1:])
    pipeline_ctx = PipelineContext(
        storage=storage, db_name=ctx.db_name, coll_name=coll_name, change_stream=cs_spec
    )

    # Bind the entry by reference; producer closes over it.
    entry_ref: dict[str, Any] = {"entry": None}

    def producer() -> list[dict[str, Any]]:
        entry = entry_ref["entry"]
        if entry is None:
            return []
        rows = storage.read_oplog(start_seq=entry.position_seq + 1, limit=200, ns_filter=_ns_filter)
        if not rows:
            return []
        events: list[dict[str, Any]] = []
        last_seen = entry.position_seq
        for seq, oplog_entry in rows:
            try:
                ev, invalidates = changestreams.project(
                    seq,
                    oplog_entry,
                    storage=storage,
                    full_document_mode=cs_spec.full_document_mode,
                    full_document_before_change_mode=cs_spec.full_document_before_change_mode,
                    scope=scope,
                )
            except changestreams.ChangeStreamFatalError as exc:
                # Best effort: surface as an empty batch and let the next
                # poll retry. A nicer surface would be a server-side error
                # cursor; defer.
                _ = exc
                last_seen = seq
                continue
            if ev is not None:
                events.append(ev)
            last_seen = seq
            if invalidates:
                events.append(changestreams.invalidate_event(seq, oplog_entry))
                entry.invalidated = True
                entry.final_event_pending = True
                break
        entry.position_seq = last_seen
        if pipeline_after_cs:
            events = apply_pipeline(events, pipeline_after_cs, pipeline_ctx)
        return events

    cursor_id = ctx.cursors.register_tailable(
        ns,
        producer,
        await_data=True,
        position_seq=start_seq - 1,
        collection_uuid=coll_uuid,
    )
    entry_ref["entry"] = ctx.cursors.get(cursor_id)
    _ = batch_size  # firstBatch is empty by design for change streams
    return {
        "cursor": {"firstBatch": [], "id": bson.Int64(cursor_id), "ns": ns},
        "operationTime": ctx.storage.current_cluster_time(),
        "ok": 1.0,
    }


# --- Authentication (SCRAM-SHA-256) ---

# MongoDB error codes used in this section.
_AUTHENTICATION_FAILED = 18
_USER_NOT_FOUND = 11
_USER_ALREADY_EXISTS = 51003
_NO_SUCH_USER_GENERIC = 11
_AUTH_NOT_REQUIRED = "Auth not required by server configuration"


def _auth_failure(msg: str = "Authentication failed.") -> dict[str, Any]:
    return {
        "ok": 0.0,
        "errmsg": msg,
        "code": _AUTHENTICATION_FAILED,
        "codeName": "AuthenticationFailed",
    }


def _payload_bytes(value: Any) -> bytes:
    """SCRAM payload arrives as BSON Binary; getter handles both Binary and bytes."""
    if isinstance(value, bson.Binary):
        return bytes(value)
    if isinstance(value, (bytes, bytearray)):
        return bytes(value)
    return b""


def _payload_binary(b: bytes) -> bson.Binary:
    return bson.Binary(b, subtype=0)


def _ensure_conn_auth(ctx: CommandContext) -> ConnectionAuth:
    if ctx.connection_auth is None:
        # Should never happen in production paths — server.py always
        # constructs one. Fail closed.
        raise RuntimeError("connection auth state missing")
    return ctx.connection_auth


def _lookup_creds(storage: Storage, db: str, username: str) -> StoredCredentials | None:
    record = storage.get_user(db, username)
    if record is None:
        return None
    creds_doc = record.get("credentials")
    if not isinstance(creds_doc, dict) or SCRAM_SHA_256 not in creds_doc:
        return None
    return StoredCredentials.from_doc(creds_doc)


def _sasl_start(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    conn = _ensure_conn_auth(ctx)
    mechanism = doc.get("mechanism", "")
    if mechanism != SCRAM_SHA_256:
        return _auth_failure(
            f"Unsupported SASL mechanism: {mechanism!r} (only {SCRAM_SHA_256} is supported)"
        )
    payload = _payload_bytes(doc.get("payload"))
    db_name = ctx.db_name or "admin"
    creds = _lookup_creds(ctx.storage, db_name, _peek_scram_username(payload))
    try:
        server_first, state = begin_scram(
            conversation_id=conn.new_conversation_id(),
            db_name=db_name,
            payload=payload,
            creds=creds,
        )
    except AuthError as exc:
        return _auth_failure(str(exc))
    conn.scram = state
    return {
        "conversationId": state.conversation_id,
        "done": False,
        "payload": _payload_binary(server_first),
        "ok": 1.0,
    }


def _peek_scram_username(payload: bytes) -> str:
    """Best-effort SCRAM `n=user` extraction for credential lookup.

    A malformed payload returns "" here, which makes ``_lookup_creds``
    return None, which makes ``begin_scram`` fabricate credentials and
    surface AuthError on the proof step — same shape as a wrong-password
    attempt. Reusing the parser keeps the lookup path single-source.
    """
    if not payload.startswith(b"n,"):
        return ""
    gs2_end = payload.find(b",", 2)
    if gs2_end < 0:
        return ""
    bare = payload[gs2_end + 1 :]
    for chunk in bare.split(b","):
        if chunk.startswith(b"n="):
            return chunk[2:].decode("utf-8", errors="replace")
    return ""


def _sasl_continue(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    conn = _ensure_conn_auth(ctx)
    state = conn.scram
    if state is None:
        return _auth_failure("No SCRAM conversation in progress")
    incoming_id = doc.get("conversationId")
    if incoming_id != state.conversation_id:
        return _auth_failure("SCRAM conversation id mismatch")
    payload = _payload_bytes(doc.get("payload"))
    try:
        server_final = continue_scram(state, payload)
    except AuthError as exc:
        conn.scram = None
        return _auth_failure(str(exc))
    # Successful proof. Mark the principal authenticated, capture their
    # role bindings for RBAC, and clear the in-flight conversation.
    # MongoDB returns done=True from the second server message
    # (skipping the spec's optional 3rd round-trip).
    principal = (state.db_name, state.username)
    if principal not in conn.authenticated_principals:
        conn.authenticated_principals.append(principal)
    record = ctx.storage.get_user(state.db_name, state.username)
    if record is not None:
        roles = record.get("roles") or []
        if isinstance(roles, list):
            conn.add_principal_roles(roles)
    conn.scram = None
    return {
        "conversationId": state.conversation_id,
        "done": True,
        "payload": _payload_binary(server_final),
        "ok": 1.0,
    }


def _create_user(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    username = doc.get("createUser")
    pwd = doc.get("pwd")
    if not isinstance(username, str) or not username:
        return {
            "ok": 0.0,
            "errmsg": "createUser: username (string) required",
            "code": 2,
            "codeName": "BadValue",
        }
    if not isinstance(pwd, str) or not pwd:
        return {
            "ok": 0.0,
            "errmsg": "createUser: pwd (string) required",
            "code": 2,
            "codeName": "BadValue",
        }
    db_name = ctx.db_name or "admin"
    creds = derive_credentials(pwd)
    roles_arg = doc.get("roles", []) or []
    normalised = _normalise_roles_arg(roles_arg, db_name)
    if normalised is None:
        return {
            "ok": 0.0,
            "errmsg": "createUser: roles must be a list of known roles",
            "code": 31,
            "codeName": "RoleNotFound",
        }
    record = {
        "_id": f"{db_name}.{username}",
        "user": username,
        "db": db_name,
        "credentials": creds.to_doc(),
        "roles": normalised,
        "mechanisms": [SCRAM_SHA_256],
    }
    added = ctx.storage.add_user(db_name, username, record, replace=False)
    if not added:
        return {
            "ok": 0.0,
            "errmsg": f'User "{username}@{db_name}" already exists',
            "code": _USER_ALREADY_EXISTS,
            "codeName": "Location51003",
        }
    return {"ok": 1.0}


def _update_user(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """Rotate a password and/or replace a user's role bindings in place.

    Real ``mongod``'s ``updateUser`` updates the existing record without
    invalidating other connections — drop+recreate would force every
    authenticated client off. We do the same: re-derive credentials when
    ``pwd`` is supplied, replace ``roles`` when supplied, leave the rest
    alone. The calling connection's effective_roles refresh live so a
    role change takes effect on the next command.
    """
    username = doc.get("updateUser")
    if not isinstance(username, str) or not username:
        return {
            "ok": 0.0,
            "errmsg": "updateUser: username (string) required",
            "code": 2,
            "codeName": "BadValue",
        }
    db_name = ctx.db_name or "admin"
    record = ctx.storage.get_user(db_name, username)
    if record is None:
        return {
            "ok": 0.0,
            "errmsg": f"User '{username}@{db_name}' not found",
            "code": _USER_NOT_FOUND,
            "codeName": "UserNotFound",
        }
    pwd = doc.get("pwd")
    roles_arg = doc.get("roles")
    if pwd is None and roles_arg is None:
        return {
            "ok": 0.0,
            "errmsg": "updateUser: nothing to update (supply pwd and/or roles)",
            "code": 2,
            "codeName": "BadValue",
        }
    if pwd is not None:
        if not isinstance(pwd, str) or not pwd:
            return {
                "ok": 0.0,
                "errmsg": "updateUser: pwd must be a non-empty string",
                "code": 2,
                "codeName": "BadValue",
            }
        record["credentials"] = derive_credentials(pwd).to_doc()
    if roles_arg is not None:
        normalised = _normalise_roles_arg(roles_arg, db_name)
        if normalised is None:
            return {
                "ok": 0.0,
                "errmsg": "updateUser: roles must be a list of known roles",
                "code": 31,
                "codeName": "RoleNotFound",
            }
        record["roles"] = normalised
    ctx.storage.add_user(db_name, username, record, replace=True)
    if roles_arg is not None:
        _refresh_effective_roles(ctx)
    return {"ok": 1.0}


def _drop_user(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    username = doc.get("dropUser")
    if not isinstance(username, str) or not username:
        return {
            "ok": 0.0,
            "errmsg": "dropUser: username (string) required",
            "code": 2,
            "codeName": "BadValue",
        }
    db_name = ctx.db_name or "admin"
    removed = ctx.storage.drop_user(db_name, username)
    if not removed:
        return {
            "ok": 0.0,
            "errmsg": f"User '{username}@{db_name}' not found",
            "code": _USER_NOT_FOUND,
            "codeName": "UserNotFound",
        }
    # Drop any active auth state for this principal on the calling
    # connection — both the principal entry and the role bindings the
    # connection inherited from this user. Other connections keep
    # theirs until they reconnect, matching mongod.
    conn = ctx.connection_auth
    if conn is not None:
        conn.authenticated_principals = [
            p for p in conn.authenticated_principals if p != (db_name, username)
        ]
        # Recompute effective roles by re-fetching the remaining
        # principals' role bindings. Cheap (small list, infrequent op).
        conn.effective_roles = []
        for p_db, p_user in conn.authenticated_principals:
            other = ctx.storage.get_user(p_db, p_user)
            if other is not None:
                roles = other.get("roles") or []
                if isinstance(roles, list):
                    conn.add_principal_roles(roles)
    return {"ok": 1.0}


def _users_info(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    db_name = ctx.db_name or "admin"
    arg = doc.get("usersInfo")
    show_credentials = bool(doc.get("showCredentials", False))

    def _public(record: dict[str, Any]) -> dict[str, Any]:
        out = dict(record)
        if not show_credentials:
            out.pop("credentials", None)
        return out

    users: list[dict[str, Any]]
    if arg == 1 or arg is True:
        # All users in this database.
        users = [_public(r) for r in ctx.storage.list_users(db_name)]
    elif isinstance(arg, str):
        record = ctx.storage.get_user(db_name, arg)
        users = [_public(record)] if record else []
    elif isinstance(arg, dict):
        # `{user: "name", db: "other"}` — single specific principal.
        u = arg.get("user")
        d = arg.get("db", db_name)
        if isinstance(u, str) and isinstance(d, str):
            record = ctx.storage.get_user(d, u)
            users = [_public(record)] if record else []
        else:
            users = []
    elif isinstance(arg, list):
        users = []
        for entry in arg:
            if isinstance(entry, str):
                record = ctx.storage.get_user(db_name, entry)
            elif isinstance(entry, dict):
                u = entry.get("user")
                d = entry.get("db", db_name)
                if isinstance(u, str) and isinstance(d, str):
                    record = ctx.storage.get_user(d, u)
                else:
                    record = None
            else:
                record = None
            if record is not None:
                users.append(_public(record))
    else:
        users = []
    return {"users": users, "ok": 1.0}


def _normalise_roles_arg(arg: Any, default_db: str) -> list[dict[str, str]] | None:
    """Coerce a ``roles`` argument into the canonical ``[{role, db}]`` shape.

    Accepts the list-of-strings shorthand (``["read", "readWrite"]`` —
    each implicitly bound to ``default_db``) and the list-of-dicts form.
    Validates role names against :data:`secantus.rbac.BUILT_IN_ROLES`.
    Returns ``None`` if any entry is unrecognised — caller surfaces a
    ``RoleNotFound`` error.
    """
    if not isinstance(arg, list):
        return None
    out: list[dict[str, str]] = []
    for entry in arg:
        if isinstance(entry, str):
            if not is_known_role(entry):
                return None
            out.append({"role": entry, "db": default_db})
            continue
        if isinstance(entry, dict):
            role = entry.get("role")
            db = entry.get("db", default_db)
            if not isinstance(role, str) or not isinstance(db, str):
                return None
            if not is_known_role(role):
                return None
            out.append({"role": role, "db": db})
            continue
        return None
    return out


def _grant_roles_to_user(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    username = doc.get("grantRolesToUser")
    if not isinstance(username, str) or not username:
        return {
            "ok": 0.0,
            "errmsg": "grantRolesToUser: username (string) required",
            "code": 2,
            "codeName": "BadValue",
        }
    db_name = ctx.db_name or "admin"
    roles_arg = doc.get("roles")
    new_roles = _normalise_roles_arg(roles_arg, db_name)
    if new_roles is None:
        return {
            "ok": 0.0,
            "errmsg": "grantRolesToUser: roles must be a list of known roles",
            "code": 31,
            "codeName": "RoleNotFound",
        }
    record = ctx.storage.get_user(db_name, username)
    if record is None:
        return {
            "ok": 0.0,
            "errmsg": f"User '{username}@{db_name}' not found",
            "code": _USER_NOT_FOUND,
            "codeName": "UserNotFound",
        }
    existing = record.get("roles") or []
    seen = {(r.get("role"), r.get("db")) for r in existing if isinstance(r, dict)}
    for r in new_roles:
        key = (r["role"], r["db"])
        if key not in seen:
            existing.append(r)
            seen.add(key)
    record["roles"] = existing
    ctx.storage.add_user(db_name, username, record, replace=True)
    # Refresh effective roles on this connection if the calling user is
    # the one that just got granted — the new privileges take effect
    # immediately, not on the next connection.
    _refresh_effective_roles(ctx)
    return {"ok": 1.0}


def _revoke_roles_from_user(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    username = doc.get("revokeRolesFromUser")
    if not isinstance(username, str) or not username:
        return {
            "ok": 0.0,
            "errmsg": "revokeRolesFromUser: username (string) required",
            "code": 2,
            "codeName": "BadValue",
        }
    db_name = ctx.db_name or "admin"
    roles_arg = doc.get("roles")
    to_revoke = _normalise_roles_arg(roles_arg, db_name)
    if to_revoke is None:
        return {
            "ok": 0.0,
            "errmsg": "revokeRolesFromUser: roles must be a list of known roles",
            "code": 31,
            "codeName": "RoleNotFound",
        }
    record = ctx.storage.get_user(db_name, username)
    if record is None:
        return {
            "ok": 0.0,
            "errmsg": f"User '{username}@{db_name}' not found",
            "code": _USER_NOT_FOUND,
            "codeName": "UserNotFound",
        }
    revoke_keys = {(r["role"], r["db"]) for r in to_revoke}
    record["roles"] = [
        r
        for r in (record.get("roles") or [])
        if isinstance(r, dict) and (r.get("role"), r.get("db")) not in revoke_keys
    ]
    ctx.storage.add_user(db_name, username, record, replace=True)
    _refresh_effective_roles(ctx)
    return {"ok": 1.0}


def _refresh_effective_roles(ctx: CommandContext) -> None:
    """Rebuild the connection's effective_roles from current user records.

    Called after `grantRolesToUser` / `revokeRolesFromUser` so the
    privilege check reflects the change immediately on this connection.
    Other connections refresh on their next reconnect.
    """
    conn = ctx.connection_auth
    if conn is None:
        return
    conn.effective_roles = []
    for p_db, p_user in conn.authenticated_principals:
        record = ctx.storage.get_user(p_db, p_user)
        if record is None:
            continue
        roles = record.get("roles") or []
        if isinstance(roles, list):
            conn.add_principal_roles(roles)


def _roles_info(doc: dict[str, Any], _ctx: CommandContext) -> dict[str, Any]:
    """Return information about built-in roles.

    Custom roles aren't supported (no `createRole` in this slice), so
    this just enumerates the built-ins. Mongod returns more fields
    (privileges, inherited roles); we surface name + db + isBuiltin
    which is enough for tooling that wants to enumerate.
    """
    arg = doc.get("rolesInfo")
    db_name = "admin"
    show_builtin = bool(doc.get("showBuiltinRoles", False))

    def _entry(role_name: str, role_db: str) -> dict[str, Any]:
        return {
            "role": role_name,
            "db": role_db,
            "isBuiltin": True,
            "roles": [],
            "inheritedRoles": [],
        }

    out: list[dict[str, Any]] = []
    if arg == 1 or arg is True:
        if show_builtin:
            for name in BUILT_IN_ROLES:
                out.append(_entry(name, db_name))
    elif isinstance(arg, str):
        if is_known_role(arg):
            out.append(_entry(arg, db_name))
    elif isinstance(arg, dict):
        role = arg.get("role")
        db = arg.get("db", db_name)
        if isinstance(role, str) and is_known_role(role):
            out.append(_entry(role, db if isinstance(db, str) else db_name))
    elif isinstance(arg, list):
        for entry in arg:
            if isinstance(entry, str) and is_known_role(entry):
                out.append(_entry(entry, db_name))
            elif isinstance(entry, dict):
                role = entry.get("role")
                db = entry.get("db", db_name)
                if isinstance(role, str) and is_known_role(role):
                    out.append(_entry(role, db if isinstance(db, str) else db_name))
    return {"roles": out, "ok": 1.0}


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
    "collMod": _coll_mod,
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
    "saslStart": _sasl_start,
    "saslContinue": _sasl_continue,
    "createUser": _create_user,
    "updateUser": _update_user,
    "dropUser": _drop_user,
    "usersInfo": _users_info,
    "grantRolesToUser": _grant_roles_to_user,
    "revokeRolesFromUser": _revoke_roles_from_user,
    "rolesInfo": _roles_info,
}

# Commands a connection may invoke before authenticating, when
# require_auth=True. The handshake plus the auth handshake itself.
_PRE_AUTH_COMMANDS = frozenset(
    {
        "hello",
        "isMaster",
        "ismaster",
        "ping",
        "buildInfo",
        "buildinfo",
        "saslStart",
        "saslContinue",
        "endSessions",
        "getLog",
        "whatsmyuri",
        "hostInfo",
    }
)


# Per-command (action, scope) for the RBAC privilege check. The dispatcher
# reads this; missing entries default to "no privilege required" so a
# user with even minimal roles can still invoke them. Adding a new
# command? Add it here (or to ``_NO_PRIVILEGE_COMMANDS`` for explicit
# bypasses) — silent permissive defaults are how mongod's auth surface
# tends to grow accidental privilege escalations, so be deliberate.
_COMMAND_ACTIONS: dict[str, tuple[str, str]] = {
    # CRUD
    "find": (A_FIND, SCOPE_COLLECTION),
    "count": (A_FIND, SCOPE_COLLECTION),
    "distinct": (A_FIND, SCOPE_COLLECTION),
    # $out/$merge stages do their own write-action checks at stage time.
    "aggregate": (A_FIND, SCOPE_COLLECTION),
    "explain": (A_FIND, SCOPE_COLLECTION),
    "insert": (A_INSERT, SCOPE_COLLECTION),
    "update": (A_UPDATE, SCOPE_COLLECTION),
    "delete": (A_REMOVE, SCOPE_COLLECTION),
    "findAndModify": (A_UPDATE, SCOPE_COLLECTION),
    "findandmodify": (A_UPDATE, SCOPE_COLLECTION),
    # Cursor lifecycle
    "killCursors": (A_KILL_CURSORS, SCOPE_COLLECTION),
    # Index management
    "listIndexes": (A_LIST_INDEXES, SCOPE_COLLECTION),
    "createIndexes": (A_CREATE_INDEX, SCOPE_COLLECTION),
    "dropIndexes": (A_DROP_INDEX, SCOPE_COLLECTION),
    # DDL
    "create": (A_CREATE_COLLECTION, SCOPE_DATABASE),
    "drop": (A_DROP_COLLECTION, SCOPE_COLLECTION),
    "dropDatabase": (A_DROP_DATABASE, SCOPE_DATABASE),
    "renameCollection": (A_RENAME_COLL_SAME_DB, SCOPE_COLLECTION),
    "collMod": (A_COLL_MOD, SCOPE_COLLECTION),
    # Listings / stats
    "listCollections": (A_LIST_COLLECTIONS, SCOPE_DATABASE),
    "listDatabases": (A_LIST_DATABASES, SCOPE_CLUSTER),
    "dbStats": (A_DB_STATS, SCOPE_DATABASE),
    "collStats": (A_COLL_STATS, SCOPE_COLLECTION),
    # User management
    "createUser": (A_CREATE_USER, SCOPE_DATABASE),
    "dropUser": (A_DROP_USER, SCOPE_DATABASE),
    "usersInfo": (A_VIEW_USER, SCOPE_DATABASE),
    "grantRolesToUser": (A_GRANT_ROLE, SCOPE_DATABASE),
    "revokeRolesFromUser": (A_REVOKE_ROLE, SCOPE_DATABASE),
    "rolesInfo": (A_VIEW_ROLE, SCOPE_DATABASE),
    "updateUser": (A_CHANGE_PASSWORD, SCOPE_DATABASE),
    # Cluster / introspection
    "serverStatus": (A_SERVER_STATUS, SCOPE_CLUSTER),
    "hostInfo": (A_HOST_INFO, SCOPE_CLUSTER),
    "getCmdLineOpts": (A_GET_CMD_LINE_OPTS, SCOPE_CLUSTER),
    "getLog": (A_GET_LOG, SCOPE_CLUSTER),
}


# Commands that intentionally skip the RBAC check even when auth is on:
# cursor continuation (the cursor was already authorized at find/aggregate
# time), session-id administrivia (no real session state), and metadata
# commands the driver depends on at every connection.
_NO_PRIVILEGE_COMMANDS = frozenset(
    {
        "getMore",
        "endSessions",
        "startSession",
        "refreshSessions",
        "ping",
        "ismaster",
        "isMaster",
        "hello",
        "buildInfo",
        "buildinfo",
        "whatsmyuri",
        "saslStart",
        "saslContinue",
        "logout",
        "connectionStatus",
        # Aborting/committing transactions are no-ops in our surrogate;
        # gating them by a privilege would only confuse drivers that do
        # protocol negotiation.
        "abortTransaction",
        "commitTransaction",
    }
)


def _resource_for_command(
    name: str, doc: dict[str, Any], default_db: str
) -> tuple[str | None, bool]:
    """Resolve the target (db, cluster_flag) the action operates on.

    For collection-level commands, ``default_db`` is the connection's
    current ``$db`` and that's the resource. For ``listCollections`` /
    ``dropDatabase`` etc. the same applies. Cluster-level commands
    (``serverStatus``, ``listDatabases``) return ``(None, True)``.

    Some commands have their target db expressed in the request rather
    than in the connection's ``$db`` — for instance ``renameCollection``
    on the admin db with a ``renameCollection: "src.coll"`` namespace
    string. We pull those out specifically to avoid mis-attributing
    privileges to the admin db.
    """
    info = _COMMAND_ACTIONS.get(name)
    if info is None:
        return default_db, False
    _action, scope = info
    if scope == SCOPE_CLUSTER:
        return None, True
    # ``renameCollection: "src.coll"`` — the source db is the namespace
    # prefix, not the connection's $db.
    if name == "renameCollection":
        ns = doc.get("renameCollection")
        if isinstance(ns, str) and "." in ns:
            return ns.split(".", 1)[0], False
    return default_db, False


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
    if (
        ctx.require_auth
        and name not in _PRE_AUTH_COMMANDS
        and (ctx.connection_auth is None or not ctx.connection_auth.is_authenticated)
    ):
        return {
            "ok": 0.0,
            "errmsg": (f"command {name} requires authentication"),
            "code": 13,
            "codeName": "Unauthorized",
        }
    # RBAC privilege check. Active only when ``--auth`` is on; without
    # auth, any client can do anything (the legacy default-allow mode).
    # Pre-auth commands (handshake, SCRAM round-trip) and explicitly
    # exempt commands (cursor continuation, session admin) bypass the
    # check. Everything else needs an action grant from the principal's
    # role bindings.
    if (
        ctx.require_auth
        and ctx.connection_auth is not None
        and ctx.connection_auth.is_authenticated
        and name not in _NO_PRIVILEGE_COMMANDS
        and name not in _PRE_AUTH_COMMANDS
    ):
        info = _COMMAND_ACTIONS.get(name)
        if info is not None:
            action, scope = info
            target_db, cluster = _resource_for_command(name, doc, ctx.db_name or "admin")
            if not check_privilege(
                ctx.connection_auth.effective_roles,
                action,
                target_db=target_db,
                cluster=cluster,
            ):
                return {
                    "ok": 0.0,
                    "errmsg": (
                        f"not authorized on {target_db or 'cluster'} to "
                        f"execute command (action: {action})"
                    ),
                    "code": 13,
                    "codeName": "Unauthorized",
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
