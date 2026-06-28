"""Embedded SQL engine (P0 spike) — compile SQL down to the Mongo engines.

This package is the bottom layer of the planned PostgreSQL-wire interface
(``tasks/sql-postgres-plan.md``). It parses a small, Postgres-flavoured SQL
subset, plans each statement into operations over the existing ``Storage`` +
operator engines (``query`` / ``update`` / ``projection``), and executes it.
There is **no wire protocol here** — the entry point is the in-process
``run_sql(storage, db, sql)`` function, exactly the P0 "embedded executor
spike" deliverable that de-risks the translate-to-Mongo-engines thesis before
any protocol work.

Scope (P0): ``CREATE TABLE`` / ``DROP TABLE`` (declared tables only),
``INSERT``, ``SELECT`` (WHERE / ORDER BY / LIMIT / OFFSET / ``COUNT(*)``),
``UPDATE``, ``DELETE``. Joins, GROUP BY, aggregates beyond ``COUNT(*)``,
reflected/jsonb tables, and the wire server are later phases.
"""

from __future__ import annotations

from secantus.sql.engine import run_sql
from secantus.sql.errors import SQLError
from secantus.sql.result import ColumnDesc, SQLResult

__all__ = ["ColumnDesc", "SQLError", "SQLResult", "run_sql"]
