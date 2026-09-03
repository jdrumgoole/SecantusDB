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


def no_active_sql_transaction(message: str) -> SQLError:
    """SQLSTATE 25P01 — a statement that requires an explicit transaction block
    was run outside one. PostgreSQL uses this for a non-holdable ``DECLARE
    CURSOR``, whose result set would be discarded the instant the implicit
    transaction committed."""
    return SQLError("25P01", message)


def insufficient_privilege(database: str, action: str) -> SQLError:
    """SQLSTATE 42501 — the connection's roles don't grant the RBAC ``action``
    the statement needs on ``database``. Raised by the per-statement gate in
    ``sql/authz.py`` when authorization is active. (#193)"""
    return SQLError("42501", f'permission denied for database "{database}" (requires {action})')


def undefined_table(name: str) -> SQLError:
    return SQLError("42P01", f'relation "{name}" does not exist')


def duplicate_table(name: str) -> SQLError:
    """42P07. PG names the relation BARE even when the statement qualified it
    (`CREATE TABLE s.t` on an existing `s.t` says `relation "t" already
    exists`, probed on 14.13), so the schema prefix of a composed catalog key
    is stripped here rather than at each call site."""
    return SQLError("42P07", f'relation "{_bare(name)}" already exists')


def _bare(name: str) -> str:
    """The relation name without the schema prefix a catalog key carries.
    A `pg_temp_<n>.` prefix goes too — PG reports temp relations bare."""
    return name.rsplit(".", 1)[-1]


#: PG's noun for each relation kind in a "does not exist" / "is not a" message.
#: `DROP TABLE nosuch` says `table "nosuch" does not exist`, not `relation`.
_KIND_NOUN = {
    "TABLE": "table",
    "VIEW": "view",
    "MATERIALIZED VIEW": "materialized view",
    "SEQUENCE": "sequence",
    "INDEX": "index",
}


def undefined_relation_of_kind(kind: str, name: str) -> SQLError:
    """42P01 (42704 for an index — PG classes a missing index as
    undefined_object, not undefined_table) naming the relation KIND the
    statement asked for. Probed against PostgreSQL 14.13."""
    noun = _KIND_NOUN.get(kind.upper(), "relation")
    sqlstate = "42704" if noun == "index" else "42P01"
    return SQLError(sqlstate, f'{noun} "{_bare(name)}" does not exist')


def wrong_object_type(name: str, kind: str) -> SQLError:
    """42809 `"x" is not a table`. PG distinguishes "the name is free" from
    "the name is taken by another KIND of relation"; answering 42P01 for the
    second told a client the object was absent when dropping it would in fact
    have needed the right DROP verb."""
    noun = _KIND_NOUN.get(kind.upper(), "relation")
    article = "an" if noun[0] in "aeiou" else "a"
    return SQLError("42809", f'"{_bare(name)}" is not {article} {noun}')


def undefined_column(name: str, relation: str | None = None) -> SQLError:
    """42703. Postgres names the RELATION when it knows it -- ``column "x" of
    relation "t" does not exist`` -- which is what ALTER TABLE reports."""
    if relation is not None:
        return SQLError("42703", f'column "{name}" of relation "{relation}" does not exist')
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
