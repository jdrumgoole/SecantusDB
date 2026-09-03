"""A fifteenth differential sweep — COPY, and a probe that was lying.

**The lesson first, because it nearly cost the whole batch.** The initial COPY
probe reported 33 of 34 scenarios matching, and that number was worthless:
psycopg's copy API lives on the CURSOR, not the connection, so
`conn.copy(...)` raised `AttributeError` — identically on both servers, which
the differential read as agreement. Twenty of the thirty-four scenarios were
vacuous. Rewritten through `conn.cursor().copy(...)`, the same corpus found
five real divergences the "clean" run had hidden completely. The probe now
reports the exception TYPE alongside the SQLSTATE and counts any scenario
where both sides raised a non-database error as VACUOUS, so this cannot
silently recur.

What the honest run found: **`FORCE_QUOTE`, `FORCE_NULL` and `FORCE_NOT_NULL`
were not supported at all**, and sqlglot is why — it does not merely mangle
them, it raises a hard `ParseError` on the entire statement, so the options
are lifted out of the SQL text by a pre-pass and re-attached to the AST.

Their rules are asymmetric in a way worth pinning: each option is valid in
exactly ONE direction, and they disagree about which. `FORCE_QUOTE` is COPY TO
only; `FORCE_NULL` and `FORCE_NOT_NULL` are COPY FROM only. All three are
CSV-only, and an unknown column is `42703`.

Two further details measured rather than assumed: a NULL is **not**
force-quoted (so `FORCE_QUOTE *` still leaves NULL as the bare marker, which
is what keeps it distinguishable from the empty string), and the HEADER line
is not force-quoted either.

Also fixed: `DELIMITER '"'` in CSV mode was accepted, producing output that
cannot be parsed back. PostgreSQL rejects it with `22023`.

Everything else in COPY came back clean across 44 scenarios — text and CSV in
both directions, HEADER, column lists, the query form, NULL markers, custom
delimiters, QUOTE/ESCAPE, ENCODING, the binary format round-trip, the `\\.`
terminator, a missing trailing newline, empty input, and the row-count tags.

Every expectation here was measured against PostgreSQL 14.13.
"""

from __future__ import annotations

import pytest

from secantus.sql.pgserver import SecantusPGServer
from secantus.storage import Storage

psycopg = pytest.importorskip("psycopg")


@pytest.fixture
def conn(tmp_path):
    st = Storage(str(tmp_path / "s15"))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    host, port = srv.address
    c = psycopg.connect(host=host, port=port, dbname="db", user="joe", autocommit=True)
    try:
        yield c
    finally:
        c.close()
        srv.stop()
        st.close()


@pytest.fixture
def seeded(conn):
    conn.execute("CREATE TABLE cp (id int, s text, n numeric, d date, b bool)")
    conn.execute(
        "INSERT INTO cp VALUES (1,'a',1.50,'2020-01-05',true),"
        "(2,'b,c',2.25,'2021-06-30',false),(3,NULL,NULL,NULL,NULL)"
    )
    return conn


def dump(c, sql):
    buf = []
    with c.cursor() as cur, cur.copy(sql) as cp:
        for d in cp:
            buf.append(bytes(d))
    return b"".join(buf).decode()


def load(c, sql, data):
    with c.cursor() as cur, cur.copy(sql) as cp:
        cp.write(data)


def sqlstate(exc):
    return getattr(getattr(exc, "diag", None), "sqlstate", None)


# --- FORCE_QUOTE (COPY TO, CSV) ---------------------------------------------- #


def test_force_quote_named_column(seeded):
    assert dump(seeded, "COPY cp TO STDOUT (FORMAT CSV, FORCE_QUOTE (s))") == (
        '1,"a",1.50,2020-01-05,t\n2,"b,c",2.25,2021-06-30,f\n3,,,,\n'
    )


def test_force_quote_star_leaves_null_bare(seeded):
    """`*` quotes every non-NULL value — and NULL stays the bare marker, which
    is the only thing keeping it distinct from the empty string."""
    assert dump(seeded, "COPY cp TO STDOUT (FORMAT CSV, FORCE_QUOTE *)") == (
        '"1","a","1.50","2020-01-05","t"\n"2","b,c","2.25","2021-06-30","f"\n"3",,,,\n'
    )


def test_force_quote_several_columns(seeded):
    assert dump(seeded, "COPY cp TO STDOUT (FORMAT CSV, FORCE_QUOTE (s, d))") == (
        '1,"a",1.50,"2020-01-05",t\n2,"b,c",2.25,"2021-06-30",f\n3,,,,\n'
    )


def test_force_quote_with_a_column_list(seeded):
    """The mask is positional, so it must follow the COPY's column list rather
    than the table's."""
    assert dump(seeded, "COPY cp (id, s) TO STDOUT (FORMAT CSV, FORCE_QUOTE (s))") == (
        '1,"a"\n2,"b,c"\n3,\n'
    )


# --- FORCE_NULL / FORCE_NOT_NULL (COPY FROM, CSV) ---------------------------- #


def test_force_null_makes_a_quoted_empty_string_null(seeded):
    seeded.execute("DELETE FROM cp")
    load(seeded, "COPY cp (id, s) FROM STDIN (FORMAT CSV, FORCE_NULL (s))", '1,""\n')
    assert seeded.execute("SELECT id, s FROM cp").fetchall() == [(1, None)]


def test_force_not_null_makes_an_empty_field_the_empty_string(seeded):
    seeded.execute("DELETE FROM cp")
    load(seeded, "COPY cp (id, s) FROM STDIN (FORMAT CSV, FORCE_NOT_NULL (s))", "1,\n")
    assert seeded.execute("SELECT id, s FROM cp").fetchall() == [(1, "")]


def test_without_the_options_the_defaults_are_the_other_way_round(seeded):
    """The pair that shows the options are doing something: a quoted empty
    field is the empty string and a bare one is NULL, until FORCE_* swaps
    them."""
    seeded.execute("DELETE FROM cp")
    load(seeded, "COPY cp (id, s) FROM STDIN (FORMAT CSV)", '1,""\n2,\n')
    assert seeded.execute("SELECT id, s FROM cp ORDER BY id").fetchall() == [(1, ""), (2, None)]


# --- each option is valid in exactly one direction --------------------------- #


@pytest.mark.parametrize(
    "sql,message",
    [
        ("COPY cp TO STDOUT (FORCE_QUOTE (s))", "available only in CSV mode"),
        ("COPY cp TO STDOUT (FORMAT CSV, FORCE_NULL (s))", "only available using COPY FROM"),
        ("COPY cp TO STDOUT (FORMAT CSV, FORCE_NOT_NULL (s))", "only available using COPY FROM"),
    ],
)
def test_force_options_rejected_on_copy_to(seeded, sql, message):
    with pytest.raises(psycopg.Error) as ei:
        dump(seeded, sql)
    assert sqlstate(ei.value) == "0A000"
    assert message in str(ei.value)


def test_force_quote_rejected_on_copy_from(seeded):
    with pytest.raises(psycopg.Error) as ei:
        load(seeded, "COPY cp FROM STDIN (FORMAT CSV, FORCE_QUOTE (s))", "1,a,1,2020-01-05,t\n")
    assert sqlstate(ei.value) == "0A000"
    assert "only available using COPY TO" in str(ei.value)


def test_force_quote_unknown_column(seeded):
    with pytest.raises(psycopg.Error) as ei:
        dump(seeded, "COPY cp TO STDOUT (FORMAT CSV, FORCE_QUOTE (nope))")
    assert sqlstate(ei.value) == "42703"
    assert 'column "nope" of relation "cp" does not exist' in str(ei.value)


# --- delimiter must differ from the quote character -------------------------- #


def test_delimiter_equal_to_quote_is_refused(seeded):
    """Accepting it produced CSV that cannot be parsed back."""
    with pytest.raises(psycopg.Error) as ei:
        dump(seeded, "COPY cp TO STDOUT (FORMAT CSV, DELIMITER '\"')")
    assert sqlstate(ei.value) == "22023"
    assert "COPY delimiter and quote must be different" in str(ei.value)


def test_a_distinct_delimiter_is_still_fine(seeded):
    assert dump(seeded, "COPY cp TO STDOUT (FORMAT CSV, DELIMITER '|')").startswith("1|a|1.50")


# --- regression cover for the parts that were already right ------------------ #


def test_csv_roundtrip(seeded):
    blob = dump(seeded, "COPY cp TO STDOUT (FORMAT CSV)")
    seeded.execute("DELETE FROM cp")
    load(seeded, "COPY cp FROM STDIN (FORMAT CSV)", blob)
    assert seeded.execute("SELECT id, s FROM cp ORDER BY id").fetchall() == [
        (1, "a"),
        (2, "b,c"),
        (3, None),
    ]


def test_binary_roundtrip(seeded):
    buf = []
    with seeded.cursor() as cur, cur.copy("COPY cp TO STDOUT (FORMAT BINARY)") as cp:
        for d in cp:
            buf.append(bytes(d))
    seeded.execute("DELETE FROM cp")
    with seeded.cursor() as cur, cur.copy("COPY cp FROM STDIN (FORMAT BINARY)") as cp:
        cp.write(b"".join(buf))
    assert seeded.execute("SELECT id, s FROM cp ORDER BY id").fetchall() == [
        (1, "a"),
        (2, "b,c"),
        (3, None),
    ]


def test_the_parse_prepass_leaves_a_plain_copy_alone(seeded):
    """`FORCE_*` is lifted out of the SQL text before sqlglot sees it; a COPY
    without them must be untouched, including one whose data contains the
    word."""
    seeded.execute("DELETE FROM cp")
    load(seeded, "COPY cp (id, s) FROM STDIN (FORMAT CSV)", "1,force_quote (x)\n")
    assert seeded.execute("SELECT s FROM cp").fetchall() == [("force_quote (x)",)]


# --- the pre-pass must not touch anything that is not a COPY option ---------- #
#
# A text rewrite that runs BEFORE the parser sees every statement is the most
# dangerous kind of change in this file: whatever it matches by accident is
# altered with no error raised anywhere. The first version of it had neither
# guard below, and `SELECT 'force_quote (x)'` evaluated to `''`.


@pytest.mark.parametrize(
    "sql,expected",
    [
        ("SELECT 'force_quote (x)'", "force_quote (x)"),
        ("SELECT 'force_null (a, b)'", "force_null (a, b)"),
        ("SELECT 'force_not_null (s)'", "force_not_null (s)"),
        ("SELECT 'FORCE_QUOTE *'", "FORCE_QUOTE *"),
    ],
)
def test_a_string_literal_is_never_rewritten(conn, sql, expected):
    assert conn.execute(sql).fetchone()[0] == expected


def test_a_stored_value_containing_the_option_survives(conn):
    conn.execute("CREATE TABLE lit (s text)")
    conn.execute("INSERT INTO lit VALUES ('force_not_null (s)')")
    assert conn.execute("SELECT s FROM lit").fetchone()[0] == "force_not_null (s)"
    assert conn.execute("SELECT s FROM lit WHERE s = 'force_not_null (s)'").fetchall() == [
        ("force_not_null (s)",)
    ]


def test_a_column_may_be_named_force_null(conn):
    conn.execute("CREATE TABLE fn (force_null int)")
    conn.execute("INSERT INTO fn VALUES (1)")
    assert conn.execute("SELECT force_null FROM fn").fetchone()[0] == 1


def test_a_copy_query_form_string_literal_is_untouched(conn):
    """The one place both hazards meet: a COPY statement (so the rewrite is
    armed) whose query carries the option name inside a literal."""
    assert dump(conn, "COPY (SELECT 'force_quote (x)') TO STDOUT") == "force_quote (x)\n"
