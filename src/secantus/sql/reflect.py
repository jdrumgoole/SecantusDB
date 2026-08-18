"""Reflect an existing Mongo collection into a schema-on-read SQL table.

A collection with no ``CREATE TABLE`` is still queryable: sample some documents,
infer a column per top-level field (and its type), and present that as a
``TableDef`` flagged ``reflected``. This is the dual-protocol payoff — data
written via ``pymongo`` is readable via SQL with no DDL. Un-sampled fields stay
queryable too (the reflected ``TableDef`` resolves any name to a like-named
field of the permissive ``any`` type); nested documents/arrays surface as
``jsonb``.
"""

from __future__ import annotations

import datetime as _dt
from typing import Any

import bson

from secantus.sql import subms
from secantus.sql.catalog import Column, TableDef

SAMPLE_SIZE = 50


def _infer_tag(value: Any) -> str:
    if isinstance(value, bool):
        return "bool"
    if isinstance(value, bson.Int64):
        return "int8"
    if isinstance(value, int):
        return "int4"
    if isinstance(value, float):
        return "float8"
    if isinstance(value, bson.Decimal128):
        return "numeric"
    if isinstance(value, _dt.datetime):
        return "timestamptz"
    if isinstance(value, bson.ObjectId):
        return "text"
    if isinstance(value, (bytes, bytearray, bson.Binary)):
        return "bytea"
    if isinstance(value, str):
        return "text"
    if isinstance(value, (dict, list)):
        return "json"
    return "any"


def reflect(storage: Any, db: str, coll: str) -> TableDef | None:
    """Build a reflected ``TableDef`` for ``db.coll``, or None if unknown."""
    docs = storage.find_matching(db, coll, {}, limit=SAMPLE_SIZE)
    if not docs:
        # An empty (but existing) collection still reflects as a bare _id table.
        if coll not in storage.list_collections(db):
            return None
        return TableDef(
            coll,
            coll,
            [Column("_id", "any", "_id", pk=True, nullable=False)],
            reflected=True,
        )

    inferred: dict[str, str] = {}
    for doc in docs:
        for key, value in doc.items():
            # A `__`-prefixed hidden storage field (an expression index's
            # materialised value, a timestamp's sub-millisecond remainder) is
            # not a column and must not be reflected as one.
            if subms.is_companion_field(key):
                continue
            if key not in inferred and value is not None:
                inferred[key] = _infer_tag(value)
    # Ensure _id leads, then first-seen order for the rest.
    ordered = (["_id"] if "_id" in inferred else []) + [k for k in inferred if k != "_id"]
    columns = [
        Column(
            name=k,
            type_tag=inferred.get(k, "any"),
            field=k,
            pk=(k == "_id"),
            nullable=(k != "_id"),
        )
        for k in ordered
    ]
    return TableDef(coll, coll, columns, reflected=True)
