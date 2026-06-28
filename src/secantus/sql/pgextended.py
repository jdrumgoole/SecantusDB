"""Extended query protocol: prepared statements, portals, and bound parameters.

The simple ``Query`` path (P1) parses and runs SQL text in one shot. The
extended protocol splits that into Parse → Bind → Describe → Execute → Sync, so
a client can prepare a statement once and run it many times with different
``$1`` parameters. This is what psycopg / JDBC use.

``ExtendedSession`` holds the per-connection prepared-statement and portal
registries and a state machine that processes one frontend message at a time,
returning the bytes to send back. On error it enters the protocol's
"skip until Sync" state, so a failed statement doesn't desync the stream.
"""

from __future__ import annotations

import logging
import struct
from dataclasses import dataclass
from typing import Any

from sqlglot import exp

from secantus.sql import engine, errors, pgwire, planner, typemap
from secantus.sql.catalog import Catalog
from secantus.sql.session import Session

logger = logging.getLogger(__name__)


@dataclass
class Prepared:
    name: str
    stmt: exp.Expression | None  # None for an empty query string
    param_oids: list[int]
    param_count: int


@dataclass
class Portal:
    name: str
    prepared: Prepared
    params: list[Any]
    bound_stmt: exp.Expression | None = None
    result: Any = None  # SQLResult, computed lazily at Execute
    offset: int = 0
    executed: bool = False


# Binary parameter decoders by Postgres type OID. Text format (the common case,
# and what our client uses) decodes to str and rides column-type coercion.
_BINARY = {
    16: lambda b: b == b"\x01",  # bool
    20: lambda b: struct.unpack("!q", b)[0],  # int8
    21: lambda b: struct.unpack("!h", b)[0],  # int2
    23: lambda b: struct.unpack("!i", b)[0],  # int4
    700: lambda b: struct.unpack("!f", b)[0],  # float4
    701: lambda b: struct.unpack("!d", b)[0],  # float8
}


def _decode_param(raw: bytes | None, fmt: int, oid: int) -> Any:
    if raw is None:
        return None
    if fmt == 0:  # text
        return raw.decode("utf-8")
    decoder = _BINARY.get(oid)
    return decoder(raw) if decoder is not None else raw.decode("utf-8", "replace")


class ExtendedSession:
    def __init__(self, storage: Any, session: Session) -> None:
        self.storage = storage
        self.session = session
        self.catalog = Catalog(storage)
        self.prepared: dict[str, Prepared] = {}
        self.portals: dict[str, Portal] = {}
        self.skip_until_sync = False

    def process(self, msg_type: str, payload: bytes) -> bytes:
        """Handle one extended-protocol message; return the bytes to send."""
        if msg_type == "S":  # Sync — always answered, clears any error state
            self.skip_until_sync = False
            return pgwire.ready_for_query(b"I")
        if self.skip_until_sync:
            return b""  # discard everything until the next Sync
        if msg_type == "H":  # Flush — we send eagerly, nothing to flush
            return b""
        try:
            if msg_type == "P":
                return self._parse(payload)
            if msg_type == "B":
                return self._bind(payload)
            if msg_type == "D":
                return self._describe(payload)
            if msg_type == "E":
                return self._execute(payload)
            if msg_type == "C":
                return self._close(payload)
            self.skip_until_sync = True
            return pgwire.error_response("08P01", f"unexpected message type '{msg_type}'")
        except errors.SQLError as exc:
            self.skip_until_sync = True
            return pgwire.error_response(exc.sqlstate, exc.message)
        except Exception as exc:  # pragma: no cover - defensive
            logger.exception("error in extended protocol")
            self.skip_until_sync = True
            return pgwire.error_response("XX000", f"internal error: {exc}")

    # -- handlers ----------------------------------------------------------- #

    def _parse(self, payload: bytes) -> bytes:
        name, query, oids = pgwire.parse_parse(payload)
        stmts = planner.parse(query)
        if len(stmts) > 1:
            raise errors.syntax_error("cannot insert multiple commands into a prepared statement")
        stmt = stmts[0] if stmts else None
        count = planner.parameter_count(stmt) if stmt is not None else 0
        self.prepared[name] = Prepared(name, stmt, oids, count)
        return pgwire.parse_complete()

    def _bind(self, payload: bytes) -> bytes:
        portal, stmt_name, formats, raw_values, _result_formats = pgwire.parse_bind(payload)
        prep = self.prepared.get(stmt_name)
        if prep is None:
            raise errors.SQLError("26000", f'prepared statement "{stmt_name}" does not exist')
        values: list[Any] = []
        for i, raw in enumerate(raw_values):
            if not formats:
                fmt = 0
            elif len(formats) == 1:
                fmt = formats[0]
            else:
                fmt = formats[i]
            oid = prep.param_oids[i] if i < len(prep.param_oids) else 0
            values.append(_decode_param(raw, fmt, oid))
        self.portals[portal] = Portal(portal, prep, values)
        return pgwire.bind_complete()

    def _describe(self, payload: bytes) -> bytes:
        kind, name = pgwire.parse_describe(payload)
        if kind == "S":
            prep = self.prepared.get(name)
            if prep is None:
                raise errors.SQLError("26000", f'prepared statement "{name}" does not exist')
            n = max(prep.param_count, len(prep.param_oids))
            oids = [prep.param_oids[i] if i < len(prep.param_oids) else 0 for i in range(n)]
            out = bytearray(pgwire.parameter_description(oids))
            stmt = (
                planner.substitute_parameters(prep.stmt, [None] * prep.param_count)
                if prep.stmt is not None
                else None
            )
            cols = self._describe_columns(stmt)
            out += self._row_desc_or_no_data(cols)
            return bytes(out)
        # Portal describe — params are bound, so describe the bound statement.
        portal = self.portals.get(name)
        if portal is None:
            raise errors.SQLError("34000", f'portal "{name}" does not exist')
        cols = self._describe_columns(self._bound(portal))
        return self._row_desc_or_no_data(cols)

    def _execute(self, payload: bytes) -> bytes:
        portal_name, max_rows = pgwire.parse_execute(payload)
        portal = self.portals.get(portal_name)
        if portal is None:
            raise errors.SQLError("34000", f'portal "{portal_name}" does not exist')
        if portal.prepared.stmt is None:
            return pgwire.empty_query_response()
        if not portal.executed:
            portal.result = engine.run_statement(
                self.storage, self.session.database, self._bound(portal), self.session, self.catalog
            )
            portal.executed = True
            portal.offset = 0
        res = portal.result
        out = bytearray()
        for pname, pvalue in res.parameter_status:
            out += pgwire.parameter_status(pname, pvalue)
        if _is_row_returning(res):
            rows = res.rows
            end = len(rows) if max_rows <= 0 else min(len(rows), portal.offset + max_rows)
            for row in rows[portal.offset : end]:
                out += pgwire.data_row([typemap.to_pg_text(v) for v in row])
            if max_rows > 0 and end < len(rows):
                portal.offset = end
                out += pgwire.portal_suspended()
            else:
                portal.offset = end
                out += pgwire.command_complete(res.command_tag)
        else:
            out += pgwire.command_complete(res.command_tag)
        return bytes(out)

    def _close(self, payload: bytes) -> bytes:
        kind, name = pgwire.parse_close(payload)
        if kind == "S":
            self.prepared.pop(name, None)
        else:
            self.portals.pop(name, None)
        return pgwire.close_complete()

    # -- helpers ------------------------------------------------------------ #

    def _bound(self, portal: Portal) -> exp.Expression | None:
        if portal.bound_stmt is None and portal.prepared.stmt is not None:
            portal.bound_stmt = planner.substitute_parameters(portal.prepared.stmt, portal.params)
        return portal.bound_stmt

    def _describe_columns(self, stmt: exp.Expression | None) -> list | None:
        if stmt is None:
            return None
        return engine.describe_statement(
            self.storage, self.session.database, stmt, self.session, self.catalog
        )

    @staticmethod
    def _row_desc_or_no_data(cols: list | None) -> bytes:
        if cols is None:
            return pgwire.no_data()
        return pgwire.row_description([(c.name, c.pg_oid) for c in cols])


def _is_row_returning(res: Any) -> bool:
    return bool(res.columns) or res.command_tag.startswith("SELECT")
