"""SQL-layer errors carrying a Postgres SQLSTATE.

Every failure surfaced to a SQL client becomes a ``SQLError``. The wire
layer (a later phase) turns these into an ``ErrorResponse('E')`` with the
``sqlstate`` + ``message`` so the connection survives — the SQL analogue of
``commands.dispatch`` turning a handler exception into ``{ok: 0, ...}`` rather
than dropping the socket. The embedded ``run_sql`` entry point just lets them
propagate.
"""

from __future__ import annotations


class SQLError(Exception):
    """A SQL error with a Postgres SQLSTATE code.

    ``sqlstate`` is the 5-char SQLSTATE (e.g. ``42P01`` undefined_table). The
    message is user-facing — it is what a real ``psql`` session would print.
    """

    def __init__(
        self,
        sqlstate: str,
        message: str,
        *,
        diag: dict[str, str] | None = None,
        position: int | None = None,
    ) -> None:
        super().__init__(message)
        self.sqlstate = sqlstate
        self.message = message
        # Optional ErrorResponse identity fields (s=schema, t=table, c=column,
        # n=constraint, d=datatype) and the 1-based statement position —
        # clients surface them via the error Diagnostics / LINE context.
        self.diag = diag or {}
        self.position = position


# A curated set of the SQLSTATEs this layer raises. Names mirror the Postgres
# error-code table so the wire layer / tests read self-documenting.
def syntax_error(message: str) -> SQLError:
    return SQLError("42601", message)


def feature_not_supported(message: str) -> SQLError:
    return SQLError("0A000", message)


def program_limit_exceeded(message: str) -> SQLError:
    """SQLSTATE 54000 — a configured resource cap was hit (too many cursors, a
    cursor result too large, a statement too long / deeply nested). (#194)"""
    return SQLError("54000", message)


def insufficient_privilege(database: str, action: str) -> SQLError:
    """SQLSTATE 42501 — the connection's roles don't grant the RBAC ``action``
    the statement needs on ``database``. Raised by the per-statement gate in
    ``sql/authz.py`` when authorization is active. (#193)"""
    return SQLError("42501", f'permission denied for database "{database}" (requires {action})')


def undefined_table(name: str) -> SQLError:
    return SQLError("42P01", f'relation "{name}" does not exist')


def duplicate_table(name: str) -> SQLError:
    return SQLError("42P07", f'relation "{name}" already exists')


def undefined_column(name: str) -> SQLError:
    return SQLError("42703", f'column "{name}" does not exist')


def not_null_violation(column: str, table_name: str | None = None) -> SQLError:
    """23502 with PG's identity fields when the table is known: s=schema,
    t=bare table name, c=column (pgjdbc's ServerErrorMessage asserts them)."""
    if table_name is None:
        return SQLError("23502", f'null value in column "{column}" violates not-null constraint')
    if table_name.startswith("pg_temp_") or "." in table_name:
        schema, bare = table_name.split(".", 1)
    else:
        schema, bare = "public", table_name
    return SQLError(
        "23502",
        f'null value in column "{column}" of relation "{bare}" violates not-null constraint',
        diag={"s": schema, "t": bare, "c": column},
    )


def unique_violation(message: str) -> SQLError:
    return SQLError("23505", message)


def foreign_key_violation(message: str) -> SQLError:
    return SQLError("23503", message)


def serialization_failure() -> SQLError:
    """SQLSTATE 40001 — the statement (or its COMMIT) lost a write-write race
    with a concurrent transaction. WiredTiger is first-updater-wins, so the
    loser surfaces the same retriable error a SERIALIZABLE Postgres would;
    the client's correct response is ROLLBACK + retry."""
    return SQLError("40001", "could not serialize access due to concurrent update")


def datatype_mismatch(message: str) -> SQLError:
    return SQLError("42804", message)
