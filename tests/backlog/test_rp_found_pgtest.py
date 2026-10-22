"""Gap tests for the Rust PG server items found on 2026-10-21 by the first
pgtest gauge run against it (CockroachDB's `pkg/sql/pgwire/testdata/pgtest`
corpus; `tasks/backlog-rust-postgres.md`, "Found by the weekly SQL gauges").

Each scenario is the shape of a corpus stanza that PostgreSQL 15.19 passes
and the Rust server does not. They speak the protocol by hand
(`pgwire_raw`), because most of them are about which message comes back
when: psycopg cannot send a Bind without an Execute, and it hides the order
of ParameterStatus and CommandComplete.

The expected answers are literals measured on a PRIVATE PostgreSQL 15.19
cluster (`initdb --locale=C -E UTF8`), because several scenarios create a
table, which the shared reference must not be given.
"""

from __future__ import annotations

from collections.abc import Callable, Iterator
from typing import Any

import pytest

from tests.backlog.pgwire_raw import SYNC, Wire, bind, describe, execute, parse, query

Connect = Callable[[], Wire]


@pytest.fixture
def wire(rust_pg: Any) -> Iterator[Connect]:
    """Fresh raw connections to this module's `secantusd-pg`, closed after."""
    opened: list[Wire] = []

    def connect() -> Wire:
        w = Wire("127.0.0.1", rust_pg.port, "test")
        opened.append(w)
        return w

    yield connect
    for w in opened:
        w.close()


# ---------------------------------------- Bind checks its parameters --


def bind_decodes_parameters(connect: Connect) -> tuple:
    """A malformed binary parameter is an error AT BIND: no BindComplete,
    and no Execute is needed for it to surface (pgtest array:8, inet:8,
    json:8, varbit:8, decimal:121, json_array:246)."""
    w = connect()
    out = []
    for sql, oids, value, run in (
        ("SELECT $1::INTERVAL[]", (), "0000000100000000010101010000000100000000", False),
        ("SELECT $1::INET", (), "", False),
        ("SELECT $1::JSONB", (), "", False),
        ("SELECT $1::VARBIT", (), "", False),
        # A numeric whose digit is out of range (0xFFF0).
        ("SELECT $1::decimal", (), "000100010000FFF00001", True),
        # A json[] whose element type OID is 0xeda, not json.
        ("SELECT $1::JSON[]", (199,), "000000000000000000000eda", True),
    ):
        msgs = [parse(sql, oids=oids), bind(params=(bytes.fromhex(value),), param_formats=(1,))]
        if run:
            msgs.append(execute())
        out.append(w.run(*msgs, SYNC))
    return tuple(out)


def bind_checks_its_shape(connect: Connect) -> tuple:
    """A Bind whose parameter count or result-format count does not match
    the statement is refused at Bind (pgtest prepare:23, errors:95)."""
    w = connect()
    return (
        w.run(
            parse("select $1", "s0", (1043, 1043, 1043)),
            bind("s0", "p0", (b"whitebear",), (0,)),
            execute("p0"),
            SYNC,
        ),
        w.run(
            parse("select $1,$2,$3", "s1", (1043, 1043, 1043)),
            bind("s1", "p1", (b"a", b"b", b"c"), (0,), (0, 1, 2, 3, 4, 5)),
            execute("p1"),
            SYNC,
        ),
    )


def cached_plan_fails_at_bind(connect: Connect) -> tuple:
    """When a column is dropped under a prepared `SELECT *`, the re-Bind
    fails with 0A000 ("cached plan must not change result type") instead of
    BindComplete (pgtest prepared_stmt_invalidation:87)."""
    w = connect()
    w.run(query("DROP TABLE IF EXISTS rp_inval"))
    w.run(query("CREATE TABLE rp_inval (id INT PRIMARY KEY, f1 TEXT, f2 TEXT)"))
    w.run(query("INSERT INTO rp_inval VALUES (1, 'x', 'hello')"))
    first = w.run(
        parse("SELECT * FROM rp_inval WHERE id = $1", "s1"),
        bind("s1", "p1", (b"1",), (0,)),
        execute("p1"),
        SYNC,
    )
    w.run(query("ALTER TABLE rp_inval DROP COLUMN f1"))
    again = w.run(bind("s1", "p1", (b"1",), (0,)), execute("p1"), SYNC)
    w.run(query("DROP TABLE rp_inval"))
    return first, again


# ----------------------------------- Parse in an aborted transaction --


def parse_in_aborted_transaction(connect: Connect) -> tuple:
    """Parse is refused with 25P02 once the transaction has failed, as every
    statement is (pgtest aborted_txn:83)."""
    w = connect()
    w.run(query("BEGIN"))
    w.run(query("SELECT 1/0"))
    out = (w.run(parse("SELECT 1", "s2"), SYNC),)
    w.run(query("ROLLBACK"))
    return out


# ------------------------------------------ parameter type resolution --


def unknown_parameters_take_their_column_types(connect: Connect) -> tuple:
    """Parameters declared `unknown` (705) are resolved from where they are
    used, so Describe reports the column types (pgtest unknown:31)."""
    w = connect()
    w.run(query("DROP TABLE IF EXISTS rp_unknown"))
    w.run(query("CREATE TABLE rp_unknown (a INT8, b TEXT, c FLOAT8)"))
    out = w.run(
        parse("INSERT INTO rp_unknown VALUES ($1, $2, $3)", "s1", (705, 705, 705)),
        describe("S", "s1"),
        bind("s1", "p1", (b"1", b"one", b"1.5"), (0, 0, 0)),
        execute("p1"),
        SYNC,
    )
    w.run(query("DROP TABLE rp_unknown"))
    return (out,)


def conflicting_parameter_uses_fail_at_parse(connect: Connect) -> tuple:
    """`$1` used as `lower($1)` and as `$1::int` is typed integer by the
    cast, so `lower(integer)` does not exist: 42883 at Parse
    (pgtest parameter_description:40)."""
    w = connect()
    return (
        w.run(
            parse("select n::int4, lower($1) from generate_series(0,$1::int) as n", "s3"),
            describe("S", "s3"),
            SYNC,
        ),
    )


def declared_parameter_type_is_checked_at_parse(connect: Connect) -> tuple:
    """`$1` declared uuid compared with a varchar column has no operator:
    42883 at Parse, before any Bind (pgtest typing:74)."""
    w = connect()
    w.run(query("DROP TABLE IF EXISTS rp_typing"))
    w.run(query("CREATE TABLE rp_typing (id INT, v VARCHAR)"))
    out = w.run(
        parse("SELECT id FROM rp_typing WHERE v = $1", "s3", (2950,)),
        bind("s3", "p1", (b"9AC39CE2-0623-4632-A965-9A51C95682D4",), (0,)),
        execute("p1"),
        SYNC,
    )
    w.run(query("DROP TABLE rp_typing"))
    return (out,)


# -------------------------------------------------- SET's reply order --


def set_reports_after_command_complete(connect: Connect) -> tuple:
    """A SET that changes a reported parameter sends CommandComplete, THEN
    ParameterStatus (pgtest param_status:7, timezone:53)."""
    w = connect()
    return (
        w.run(query("SET application_name = 'pgtest'")),
        w.run(query('SET TIME ZONE "America/Chicago"')),
    )


def startup_parameter_order(connect: Connect) -> tuple:
    """The ParameterStatus messages after startup arrive in one fixed order,
    the same on every connection."""
    orders = set()
    for _ in range(8):
        orders.add(tuple(m[1] for m in connect().startup if m[0] == "ParameterStatus"))
    return (len(orders),)


# ------------------------------------------- RowDescription type size --


def fixed_length_types_report_their_size(connect: Connect) -> tuple:
    """RowDescription carries the type's `typlen`: 4 for void, an enum and
    the reg* types, not -1 (pgtest void:8, enum:64, oid:10)."""
    w = connect()
    w.run(query("DROP TYPE IF EXISTS rp_te"))
    w.run(query("CREATE TYPE rp_te AS ENUM ('hi')"))
    out = tuple(
        _sizes(w.run(query(sql)))
        for sql in (
            "SELECT pg_sleep(0)",
            "SELECT 'hi'::rp_te AS te",
            "SELECT 'pg_class'::regclass, 'int4'::regtype, 'now'::regproc",
        )
    )
    w.run(query("DROP TYPE rp_te"))
    return out


def _sizes(replies: list[tuple]) -> list[tuple]:
    """Each RowDescription column as (name, typlen): an enum's type OID is
    installation-specific."""
    return [(c[0], c[3]) for m in replies if m[0] == "RowDescription" for c in m[1]]


# --------------------------------------------- reg* parameter values --


def reg_parameters_render_as_text(connect: Connect) -> tuple:
    """Binary regclass / regnamespace / regproc / regtype / oid parameters
    come back with their own types, rendered in the TEXT format the client
    asked for (pgtest oid:10)."""
    w = connect()
    vals = tuple(
        bytes.fromhex(h) for h in ("01000029", "0100002a", "0100002b", "0100002c", "ffffffff")
    )
    return _without_sizes(
        w.run(
            parse("SELECT $1, $2, $3, $4, $5", "", (2205, 4089, 24, 2206, 26)),
            describe("S"),
            bind(params=vals, param_formats=(1, 1, 1, 1, 1)),
            execute(),
            SYNC,
        )
    )


def _without_sizes(replies: list[tuple]) -> tuple:
    """The replies with each column's typlen dropped, which is RP item of its
    own (`fixed_length_types_report_their_size`)."""
    out = []
    for m in replies:
        if m[0] == "RowDescription":
            m = (m[0], tuple((c[0], c[1], c[2], c[4]) for c in m[1]))
        out.append(m)
    return (out,)


# ------------------------------------------------------ extra_float_digits --


def extra_float_digits_below_one(connect: Connect) -> tuple:
    """`extra_float_digits` below 1 rounds float output to
    FLT_DIG / DBL_DIG + extra_float_digits significant digits (pgtest
    float:108)."""
    w = connect()
    out = []
    for n in (-1, 0, -15):
        w.run(query(f"SET extra_float_digits = {n}"))
        out.append(w.run(query("SELECT (1/3.0)::float4, (1/3.0)::float8")))
    return tuple(out)


# ------------------------------------------------- BEGIN inside a block --


def begin_inside_a_transaction_warns(connect: Connect) -> tuple:
    """A second BEGIN is allowed with WARNING 25001 "there is already a
    transaction in progress", by simple query and by a prepared statement
    (pgtest implicit_txn:49)."""
    w = connect()
    w.run(query("BEGIN"))
    out = (
        w.run(query("BEGIN")),
        w.run(parse("BEGIN", "b2"), bind("b2", "p2"), execute("p2"), SYNC),
    )
    w.run(query("ROLLBACK"))
    return out


# ---------------------------------------------------------- "char" zero --


def char_of_zero_is_a_zero_byte(connect: Connect) -> tuple:
    """`0::"char"` is the zero byte: binary 00, not NULL (pgtest char:201)."""
    w = connect()
    return (
        w.run(parse('SELECT 0::"char"'), bind(result_formats=(1,)), execute(), SYNC),
        w.run(parse('SELECT 0::"char" IS NULL'), bind(), execute(), SYNC),
    )


# --------------------------------------------- COPY ... CSV HEADER count --


def copy_header_is_not_a_row(connect: Connect) -> tuple:
    """`COPY ... TO STDOUT CSV HEADER` counts rows, not the header line:
    COPY 4 for four rows (pgtest copy:71)."""
    w = connect()
    w.run(query("DROP TABLE IF EXISTS rp_copy"))
    w.run(query("CREATE TABLE rp_copy (i INT8, t TEXT)"))
    w.run(query("INSERT INTO rp_copy VALUES (1, 'blah'), (2, ''), (3, NULL), (4, '\"\"')"))
    out = (
        w.run(query("COPY (SELECT * FROM rp_copy ORDER BY i) TO STDOUT CSV HEADER")),
        w.run(query("COPY rp_copy TO STDOUT CSV HEADER")),
    )
    w.run(query("DROP TABLE rp_copy"))
    return tuple([m for m in r if m[0] != "CopyData"] for r in out)


# ------------------------------------------- a view column's attnum --


def view_columns_name_their_base_columns(connect: Connect) -> tuple:
    """A simple view's columns report the base table's attnum in
    RowDescription, as plain table columns do (pgtest row_description:125)."""
    w = connect()
    w.run(query("DROP VIEW IF EXISTS rp_v"))
    w.run(query("DROP TABLE IF EXISTS rp_tab2, rp_tab1"))
    w.run(query("CREATE TABLE rp_tab1 (a INT8 PRIMARY KEY, b INT8)"))
    w.run(query("CREATE TABLE rp_tab2 (c INT8 PRIMARY KEY, tab1_a INT8 REFERENCES rp_tab1(a))"))
    w.run(
        query(
            "CREATE VIEW rp_v (v1, v2) AS SELECT a, tab1_a"
            " FROM rp_tab1 JOIN rp_tab2 ON rp_tab1.a = rp_tab2.tab1_a"
        )
    )
    out = (w.run(query("SELECT * FROM rp_v WHERE v1 = 1")),)
    w.run(query("DROP VIEW rp_v"))
    w.run(query("DROP TABLE rp_tab2, rp_tab1"))
    return out


# ------------------------------------------------------ empty jsonpath --


def empty_jsonpath_is_a_syntax_error(connect: Connect) -> tuple:
    """`''::jsonpath` is 42601 (pgtest jsonpath:5)."""
    w = connect()
    return (w.run(query("SELECT ''::JSONPATH")),)


# --------------------------------------- Bind onto an existing portal --


def bind_to_an_existing_portal(connect: Connect) -> tuple:
    """Binding to a portal name already open in the transaction is 42P03
    (pgtest multiple_active_portals:137)."""
    w = connect()
    w.run(query("BEGIN"))
    first = w.run(
        parse("SELECT * FROM generate_series(1, 5)", "q1"), bind("q1", "p2"), execute("p2", 2), SYNC
    )
    again = w.run(
        parse("SELECT * FROM generate_series(1, 5)", "q3"), bind("q3", "p2"), execute("p2", 2), SYNC
    )
    w.run(query("ROLLBACK"))
    return first, again


# ------------------------------------------ an unknown portal's name --


def unknown_portal_error_names_it(connect: Connect) -> tuple:
    """Executing a portal that was never bound is 34000, and the message
    names the portal the client sent: `portal "" does not exist` for the
    unnamed one (pgtest portals:24)."""
    w = connect()
    out = []
    for name in ("", "nope"):
        replies = w.run(execute(name), SYNC)
        out.append((replies, w.errors[-1]["M"]))
    return tuple(out)


# ----------------------------------------------- an unknown type name --


def unknown_type_fails_at_parse(connect: Connect) -> tuple:
    """`$1::GEOMETRY` without PostGIS is 42704 at Parse (pgtest spatial:8)."""
    w = connect()
    return (w.run(parse("SELECT $1::GEOMETRY"), SYNC),)


# Measured on PostgreSQL 15.19 (a private cluster, initdb --locale=C -E UTF8).
EXPECTED: dict[str, tuple] = {
    "bind_decodes_parameters": (
        [("ParseComplete",), ("ErrorResponse", "08P01"), ("ReadyForQuery", "I")],
        [("ParseComplete",), ("ErrorResponse", "08P01"), ("ReadyForQuery", "I")],
        [("ParseComplete",), ("ErrorResponse", "08P01"), ("ReadyForQuery", "I")],
        [("ParseComplete",), ("ErrorResponse", "08P01"), ("ReadyForQuery", "I")],
        [("ParseComplete",), ("ErrorResponse", "22P03"), ("ReadyForQuery", "I")],
        [("ParseComplete",), ("ErrorResponse", "42804"), ("ReadyForQuery", "I")],
    ),
    "bind_checks_its_shape": (
        [("ParseComplete",), ("ErrorResponse", "08P01"), ("ReadyForQuery", "I")],
        [("ParseComplete",), ("ErrorResponse", "08P01"), ("ReadyForQuery", "I")],
    ),
    "cached_plan_fails_at_bind": (
        [
            ("ParseComplete",),
            ("BindComplete",),
            ("DataRow", ("31", "78", "68656c6c6f")),
            ("CommandComplete", "SELECT 1"),
            ("ReadyForQuery", "I"),
        ],
        [("ErrorResponse", "0A000"), ("ReadyForQuery", "I")],
    ),
    "parse_in_aborted_transaction": ([("ErrorResponse", "25P02"), ("ReadyForQuery", "E")],),
    "unknown_parameters_take_their_column_types": (
        [
            ("ParseComplete",),
            ("ParameterDescription", (20, 25, 701)),
            ("NoData",),
            ("BindComplete",),
            ("CommandComplete", "INSERT 0 1"),
            ("ReadyForQuery", "I"),
        ],
    ),
    "conflicting_parameter_uses_fail_at_parse": (
        [("ErrorResponse", "42883"), ("ReadyForQuery", "I")],
    ),
    "set_reports_after_command_complete": (
        [
            ("CommandComplete", "SET"),
            ("ParameterStatus", "application_name", "pgtest"),
            ("ReadyForQuery", "I"),
        ],
        [
            ("CommandComplete", "SET"),
            ("ParameterStatus", "TimeZone", "America/Chicago"),
            ("ReadyForQuery", "I"),
        ],
    ),
    "startup_parameter_order": (1,),
    "fixed_length_types_report_their_size": (
        [("pg_sleep", 4)],
        [("te", 4)],
        [("regclass", 4), ("regtype", 4), ("regproc", 4)],
    ),
    "reg_parameters_render_as_text": (
        [
            ("ParseComplete",),
            ("ParameterDescription", (2205, 4089, 24, 2206, 26)),
            (
                "RowDescription",
                (
                    ("?column?", 0, 2205, 0),
                    ("?column?", 0, 4089, 0),
                    ("?column?", 0, 24, 0),
                    ("?column?", 0, 2206, 0),
                    ("?column?", 0, 26, 0),
                ),
            ),
            ("BindComplete",),
            (
                "DataRow",
                (
                    "3136373737323537",
                    "3136373737323538",
                    "3136373737323539",
                    "3136373737323630",
                    "34323934393637323935",
                ),
            ),
            ("CommandComplete", "SELECT 1"),
            ("ReadyForQuery", "I"),
        ],
    ),
    "extra_float_digits_below_one": (
        [
            ("RowDescription", (("float4", 0, 700, 4, 0), ("float8", 0, 701, 8, 0))),
            ("DataRow", ("302e3333333333", "302e3333333333333333333333333333")),
            ("CommandComplete", "SELECT 1"),
            ("ReadyForQuery", "I"),
        ],
        [
            ("RowDescription", (("float4", 0, 700, 4, 0), ("float8", 0, 701, 8, 0))),
            ("DataRow", ("302e333333333333", "302e333333333333333333333333333333")),
            ("CommandComplete", "SELECT 1"),
            ("ReadyForQuery", "I"),
        ],
        [
            ("RowDescription", (("float4", 0, 700, 4, 0), ("float8", 0, 701, 8, 0))),
            ("DataRow", ("302e33", "302e33")),
            ("CommandComplete", "SELECT 1"),
            ("ReadyForQuery", "I"),
        ],
    ),
    "begin_inside_a_transaction_warns": (
        [
            ("NoticeResponse", "WARNING", "25001"),
            ("CommandComplete", "BEGIN"),
            ("ReadyForQuery", "T"),
        ],
        [
            ("ParseComplete",),
            ("BindComplete",),
            ("NoticeResponse", "WARNING", "25001"),
            ("CommandComplete", "BEGIN"),
            ("ReadyForQuery", "T"),
        ],
    ),
    "char_of_zero_is_a_zero_byte": (
        [
            ("ParseComplete",),
            ("BindComplete",),
            ("DataRow", ("00",)),
            ("CommandComplete", "SELECT 1"),
            ("ReadyForQuery", "I"),
        ],
        [
            ("ParseComplete",),
            ("BindComplete",),
            ("DataRow", ("66",)),
            ("CommandComplete", "SELECT 1"),
            ("ReadyForQuery", "I"),
        ],
    ),
    "copy_header_is_not_a_row": (
        [
            ("CopyOutResponse",),
            ("CopyDone",),
            ("CommandComplete", "COPY 4"),
            ("ReadyForQuery", "I"),
        ],
        [
            ("CopyOutResponse",),
            ("CopyDone",),
            ("CommandComplete", "COPY 4"),
            ("ReadyForQuery", "I"),
        ],
    ),
    "view_columns_name_their_base_columns": (
        [
            ("RowDescription", (("v1", 1, 20, 8, 0), ("v2", 2, 20, 8, 0))),
            ("CommandComplete", "SELECT 0"),
            ("ReadyForQuery", "I"),
        ],
    ),
    "empty_jsonpath_is_a_syntax_error": ([("ErrorResponse", "22P02"), ("ReadyForQuery", "I")],),
    "bind_to_an_existing_portal": (
        [
            ("ParseComplete",),
            ("BindComplete",),
            ("DataRow", ("31",)),
            ("DataRow", ("32",)),
            ("PortalSuspended",),
            ("ReadyForQuery", "T"),
        ],
        [("ParseComplete",), ("ErrorResponse", "42P03"), ("ReadyForQuery", "E")],
    ),
    "unknown_type_fails_at_parse": ([("ErrorResponse", "42704"), ("ReadyForQuery", "I")],),
    "declared_parameter_type_is_checked_at_parse": (
        [("ErrorResponse", "42883"), ("ReadyForQuery", "I")],
    ),
    "unknown_portal_error_names_it": (
        ([("ErrorResponse", "34000"), ("ReadyForQuery", "I")], 'portal "" does not exist'),
        ([("ErrorResponse", "34000"), ("ReadyForQuery", "I")], 'portal "nope" does not exist'),
    ),
}


def _check(name: str, fn: Callable[[Connect], tuple], connect: Connect) -> None:
    assert fn(connect) == EXPECTED[name]


@pytest.mark.backlog_fixed("RP-078")
def test_bind_decodes_parameters(wire: Connect) -> None:
    _check("bind_decodes_parameters", bind_decodes_parameters, wire)


@pytest.mark.backlog_fixed("RP-078")
def test_bind_checks_its_shape(wire: Connect) -> None:
    _check("bind_checks_its_shape", bind_checks_its_shape, wire)


@pytest.mark.backlog_fixed("RP-078")
def test_cached_plan_fails_at_bind(wire: Connect) -> None:
    _check("cached_plan_fails_at_bind", cached_plan_fails_at_bind, wire)


@pytest.mark.backlog_fixed("RP-079")
def test_parse_in_aborted_transaction(wire: Connect) -> None:
    _check("parse_in_aborted_transaction", parse_in_aborted_transaction, wire)


@pytest.mark.backlog_fixed("RP-080")
def test_unknown_parameters_take_their_column_types(wire: Connect) -> None:
    _check(
        "unknown_parameters_take_their_column_types",
        unknown_parameters_take_their_column_types,
        wire,
    )


@pytest.mark.backlog_fixed("RP-080")
def test_conflicting_parameter_uses_fail_at_parse(wire: Connect) -> None:
    _check(
        "conflicting_parameter_uses_fail_at_parse", conflicting_parameter_uses_fail_at_parse, wire
    )


@pytest.mark.backlog_fixed("RP-081")
def test_set_reports_after_command_complete(wire: Connect) -> None:
    _check("set_reports_after_command_complete", set_reports_after_command_complete, wire)


@pytest.mark.backlog_fixed("RP-081")
def test_startup_parameter_order(wire: Connect) -> None:
    _check("startup_parameter_order", startup_parameter_order, wire)


@pytest.mark.backlog_fixed("RP-082")
def test_fixed_length_types_report_their_size(wire: Connect) -> None:
    _check("fixed_length_types_report_their_size", fixed_length_types_report_their_size, wire)


@pytest.mark.backlog_fixed("RP-083")
def test_reg_parameters_render_as_text(wire: Connect) -> None:
    _check("reg_parameters_render_as_text", reg_parameters_render_as_text, wire)


@pytest.mark.backlog_fixed("RP-068")
def test_extra_float_digits_below_one(wire: Connect) -> None:
    _check("extra_float_digits_below_one", extra_float_digits_below_one, wire)


@pytest.mark.backlog_fixed("RP-084")
def test_begin_inside_a_transaction_warns(wire: Connect) -> None:
    _check("begin_inside_a_transaction_warns", begin_inside_a_transaction_warns, wire)


@pytest.mark.backlog_fixed("RP-085")
def test_char_of_zero_is_a_zero_byte(wire: Connect) -> None:
    _check("char_of_zero_is_a_zero_byte", char_of_zero_is_a_zero_byte, wire)


@pytest.mark.backlog_fixed("RP-086")
def test_copy_header_is_not_a_row(wire: Connect) -> None:
    _check("copy_header_is_not_a_row", copy_header_is_not_a_row, wire)


@pytest.mark.backlog_fixed("RP-087")
def test_view_columns_name_their_base_columns(wire: Connect) -> None:
    _check("view_columns_name_their_base_columns", view_columns_name_their_base_columns, wire)


@pytest.mark.backlog_fixed("RP-088")
def test_empty_jsonpath_is_a_syntax_error(wire: Connect) -> None:
    _check("empty_jsonpath_is_a_syntax_error", empty_jsonpath_is_a_syntax_error, wire)


@pytest.mark.backlog_fixed("RP-089")
def test_bind_to_an_existing_portal(wire: Connect) -> None:
    _check("bind_to_an_existing_portal", bind_to_an_existing_portal, wire)


@pytest.mark.backlog_fixed("RP-090")
def test_unknown_type_fails_at_parse(wire: Connect) -> None:
    _check("unknown_type_fails_at_parse", unknown_type_fails_at_parse, wire)


@pytest.mark.backlog_fixed("RP-080")
def test_declared_parameter_type_is_checked_at_parse(wire: Connect) -> None:
    _check(
        "declared_parameter_type_is_checked_at_parse",
        declared_parameter_type_is_checked_at_parse,
        wire,
    )


@pytest.mark.backlog_fixed("RP-091")
def test_unknown_portal_error_names_it(wire: Connect) -> None:
    _check("unknown_portal_error_names_it", unknown_portal_error_names_it, wire)
