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

    def __init__(self, sqlstate: str, message: str) -> None:
        super().__init__(message)
        self.sqlstate = sqlstate
        self.message = message


# A curated set of the SQLSTATEs this layer raises. Names mirror the Postgres
# error-code table so the wire layer / tests read self-documenting.
def syntax_error(message: str) -> SQLError:
    return SQLError("42601", message)


def feature_not_supported(message: str) -> SQLError:
    return SQLError("0A000", message)


def undefined_table(name: str) -> SQLError:
    return SQLError("42P01", f'relation "{name}" does not exist')


def duplicate_table(name: str) -> SQLError:
    return SQLError("42P07", f'relation "{name}" already exists')


def undefined_column(name: str) -> SQLError:
    return SQLError("42703", f'column "{name}" does not exist')


def not_null_violation(column: str) -> SQLError:
    return SQLError(
        "23502", f'null value in column "{column}" violates not-null constraint'
    )


def unique_violation(message: str) -> SQLError:
    return SQLError("23505", message)


def datatype_mismatch(message: str) -> SQLError:
    return SQLError("42804", message)
