"""Result shapes returned by the embedded SQL engine."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass
class ColumnDesc:
    """One output column: its name, internal type tag, and Postgres OID.

    The OID is what the wire layer will put in ``RowDescription``; carried here
    so the embedded result already describes itself the way the protocol needs.
    """

    name: str
    type_tag: str
    pg_oid: int
    # PG type modifier (``atttypmod``): n+4 for varchar(n)/bpchar(n),
    # ((p<<16)|(s&0x7FF))+4 for numeric(p,s), the bare length/precision for
    # bit/varbit/time-family types, -1 when the type carries none.
    typmod: int = -1
    # The base column this output column came from, as RowDescription reports
    # it: the source table's pg_class oid and the column's 1-based attnum, or
    # 0/0 for anything computed. A JDBC updatable ResultSet resolves column
    # names through these — with 0/0 it cannot, and builds ``SET "" = ?``.
    table_oid: int = 0
    attnum: int = 0


@dataclass
class SQLResult:
    """The outcome of one SQL statement.

    ``command_tag`` is the Postgres ``CommandComplete`` tag (``SELECT 3``,
    ``INSERT 0 2``, ``UPDATE 1``, ``DELETE 0``, ``CREATE TABLE``). For SELECT,
    ``columns`` describes the result shape and ``rows`` holds the data as plain
    Python tuples in column order; for DML they are empty and ``rowcount`` is
    the affected-row count.
    """

    command_tag: str
    columns: list[ColumnDesc] = field(default_factory=list)
    rows: list[tuple[Any, ...]] = field(default_factory=list)
    rowcount: int = 0
    # GUCs to echo back as ParameterStatus messages (set by ``SET`` on a
    # reportable parameter). Empty for everything else.
    parameter_status: list[tuple[str, str]] = field(default_factory=list)
    # NOTICE/WARNING messages to send ahead of the result (``DO $$ … RAISE
    # NOTICE …$$``): ``(severity, message)`` pairs.
    notices: list[tuple[str, str]] = field(default_factory=list)
