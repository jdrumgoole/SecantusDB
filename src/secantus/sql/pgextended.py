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

import datetime as _dt
import logging
import struct
from dataclasses import dataclass, field
from decimal import Decimal
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
    # Bind's result-format codes (0=text, 1=binary): [] all-text, [c] all-c, else per-col.
    result_formats: list[int] = field(default_factory=list)


# Postgres binary timestamps count microseconds from 2000-01-01 00:00:00 UTC;
# dates count days from the same epoch.
_PG_EPOCH = _dt.datetime(2000, 1, 1, tzinfo=_dt.timezone.utc)
_PG_EPOCH_DATE = _dt.date(2000, 1, 1)


def _decode_numeric(b: bytes) -> Decimal:
    """Decode Postgres' binary ``numeric`` (base-10000 digits)."""
    ndigits, weight, sign, dscale = struct.unpack_from("!HhHH", b, 0)
    digits = [struct.unpack_from("!H", b, 8 + 2 * i)[0] for i in range(ndigits)]
    if sign == 0xC000:  # NaN
        return Decimal("NaN")
    s = "".join(f"{d:04d}" for d in digits) or "0"
    # The first digit group sits at base-10000 position ``weight``.
    value = Decimal(s).scaleb((weight - (ndigits - 1)) * 4) if digits else Decimal(0)
    if sign == 0x4000:
        value = -value
    # Round to the declared display scale so 19.99 doesn't become 19.9900...
    return value.quantize(Decimal(1).scaleb(-dscale)) if dscale else value


def _decode_timestamptz(b: bytes) -> _dt.datetime:
    return _PG_EPOCH + _dt.timedelta(microseconds=struct.unpack("!q", b)[0])


# Binary parameter decoders by Postgres type OID. The text format (fmt 0) decodes
# to str and rides column-type coercion; libpq clients (psycopg) send many types
# in binary, so the common set is decoded here to the native Python value.
_BINARY = {
    16: lambda b: b == b"\x01",  # bool
    17: lambda b: bytes(b),  # bytea
    20: lambda b: struct.unpack("!q", b)[0],  # int8
    21: lambda b: struct.unpack("!h", b)[0],  # int2
    23: lambda b: struct.unpack("!i", b)[0],  # int4
    25: lambda b: b.decode("utf-8"),  # text
    700: lambda b: struct.unpack("!f", b)[0],  # float4
    701: lambda b: struct.unpack("!d", b)[0],  # float8
    1043: lambda b: b.decode("utf-8"),  # varchar
    1082: lambda b: _PG_EPOCH_DATE + _dt.timedelta(days=struct.unpack("!i", b)[0]),  # date
    1114: _decode_timestamptz,  # timestamp (no tz) — same wire layout
    1184: _decode_timestamptz,  # timestamptz
    1700: _decode_numeric,  # numeric
}


def _encode_timestamptz(value: Any) -> bytes:
    if not isinstance(value, _dt.datetime):
        return str(value).encode("utf-8")
    if value.tzinfo is None:
        value = value.replace(tzinfo=_dt.timezone.utc)
    return struct.pack("!q", round((value - _PG_EPOCH).total_seconds() * 1_000_000))


def _encode_date(value: Any) -> bytes:
    if isinstance(value, _dt.datetime):
        value = value.date()
    if isinstance(value, _dt.date):
        return struct.pack("!i", (value - _PG_EPOCH_DATE).days)
    return str(value).encode("utf-8")


def _encode_numeric(value: Any) -> bytes:
    """Encode a Decimal as Postgres' binary ``numeric`` (base-10000 digits)."""
    d = value if isinstance(value, Decimal) else Decimal(str(value))
    if d.is_nan():
        return struct.pack("!HhHH", 0, 0, 0xC000, 0)
    sign = 0x4000 if d < 0 else 0x0000
    d = -d if d < 0 else d
    _, digits_t, exp_t = d.as_tuple()
    dscale = -exp_t if exp_t < 0 else 0
    if exp_t >= 0:
        intg, frac = "".join(map(str, digits_t)) + "0" * exp_t, ""
    else:
        ds = "".join(map(str, digits_t))
        if len(ds) <= -exp_t:
            ds = "0" * (-exp_t - len(ds) + 1) + ds
        intg, frac = ds[:exp_t], ds[exp_t:]
    intg = intg or "0"
    intg = "0" * ((-len(intg)) % 4) + intg
    frac = frac + "0" * ((-len(frac)) % 4)
    groups_int = [int(intg[i : i + 4]) for i in range(0, len(intg), 4)]
    digits = groups_int + [int(frac[i : i + 4]) for i in range(0, len(frac), 4)]
    weight = len(groups_int) - 1
    while len(digits) > 1 and digits[0] == 0:  # strip leading zero groups
        digits.pop(0)
        weight -= 1
    while digits and digits[-1] == 0:  # strip trailing zero groups
        digits.pop()
    if not digits:
        return struct.pack("!HhHH", 0, 0, sign, dscale)
    out = bytearray(struct.pack("!HhHH", len(digits), weight, sign, dscale))
    for g in digits:
        out += struct.pack("!H", g)
    return bytes(out)


# Binary *output* encoders by Postgres type OID — the inverse of ``_BINARY``. The
# value has already been ``to_py``-normalised. Text/varchar/json binary is just
# the UTF-8 text bytes; jsonb (3802) prefixes a version byte.
_OUT_BINARY = {
    16: lambda v: b"\x01" if v else b"\x00",  # bool
    17: lambda v: bytes(v),  # bytea
    20: lambda v: struct.pack("!q", int(v)),  # int8
    21: lambda v: struct.pack("!h", int(v)),  # int2
    23: lambda v: struct.pack("!i", int(v)),  # int4
    700: lambda v: struct.pack("!f", float(v)),  # float4
    701: lambda v: struct.pack("!d", float(v)),  # float8
    1082: _encode_date,  # date
    1114: _encode_timestamptz,  # timestamp (no tz)
    1184: _encode_timestamptz,  # timestamptz
    1700: _encode_numeric,  # numeric
}


def _encode_value(value: Any, oid: int, tag: str | None) -> bytes | None:
    """Binary-encode a result value for ``oid``; None stays None (NULL on the wire)."""
    if value is None:
        return None
    enc = _OUT_BINARY.get(oid)
    if enc is not None:
        return enc(value)
    if oid == 3802:  # jsonb: 1-byte version header + UTF-8 JSON text
        return b"\x01" + (typemap.to_pg_text(value, tag) or b"")
    # text / varchar / unknown: the binary form equals the text bytes.
    return typemap.to_pg_text(value, tag) or b""


def _result_value(value: Any, fmt: int, oid: int, tag: str | None) -> bytes | None:
    return _encode_value(value, oid, tag) if fmt == 1 else typemap.to_pg_text(value, tag)


def _column_formats(result_formats: list[int], ncols: int) -> list[int]:
    """Expand Bind's result-format codes to one per column."""
    if not result_formats:
        return [0] * ncols
    if len(result_formats) == 1:
        return [result_formats[0]] * ncols
    return [result_formats[i] if i < len(result_formats) else 0 for i in range(ncols)]


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
            return pgwire.ready_for_query(self.session.txn_status())
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
        except Exception:  # pragma: no cover - defensive
            logger.exception("error in extended protocol")
            self.skip_until_sync = True
            # Generic wire message; full detail stays in the server log — don't
            # leak the raw Python exception text to the client. (§I17)
            return pgwire.error_response("XX000", "internal error")

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
        portal, stmt_name, formats, raw_values, result_formats = pgwire.parse_bind(payload)
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
        self.portals[portal] = Portal(portal, prep, values, result_formats=result_formats)
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
        # Portal describe — params are bound, so describe the bound statement, and
        # report the per-column result formats the client asked for in Bind.
        portal = self.portals.get(name)
        if portal is None:
            raise errors.SQLError("34000", f'portal "{name}" does not exist')
        cols = self._describe_columns(self._bound(portal))
        formats = _column_formats(portal.result_formats, len(cols)) if cols else None
        return self._row_desc_or_no_data(cols, formats)

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
            cols = res.columns
            fmts = _column_formats(portal.result_formats, len(cols))
            end = len(rows) if max_rows <= 0 else min(len(rows), portal.offset + max_rows)
            for row in rows[portal.offset : end]:
                out += pgwire.data_row(
                    [
                        _result_value(v, f, c.pg_oid, c.type_tag)
                        for v, f, c in zip(row, fmts, cols, strict=False)
                    ]
                )
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
    def _row_desc_or_no_data(cols: list | None, formats: list[int] | None = None) -> bytes:
        if cols is None:
            return pgwire.no_data()
        return pgwire.row_description([(c.name, c.pg_oid) for c in cols], formats)


def _is_row_returning(res: Any) -> bool:
    return bool(res.columns) or res.command_tag.startswith("SELECT")
