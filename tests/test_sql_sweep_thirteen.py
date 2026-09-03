"""A thirteenth differential sweep — ``character(n)``, the blank-padded type.

`char(n)` is the one PostgreSQL string type whose STORED form and its form in
every expression differ, and SecantusDB got the two halves from two different
models. The column path had it right — store unpadded, pad on the way out
(`typemap.blank_pad`) — but the CAST path padded eagerly into the value, so
everything downstream of `::char(n)` saw blanks PostgreSQL had already
stripped: `'a'::char(3) || '|'` was `'a  |'` and `length('a'::char(3))` was 3.
Eighteen of twenty-one probed shapes diverged.

The rule, measured rather than assumed: **a bpchar-to-text conversion strips
trailing blanks, and almost everything is a text conversion.** `length`,
`upper`, `md5`, `left`, `position`, `||` and `::text` all see `'ab'`. What sees
the padded `'ab   '` is the narrow set of functions that take the value through
the type's OUTPUT function instead — `octet_length`, `concat`, `concat_ws`,
`format`, `to_json`, `to_jsonb` — plus `::bytea`. So the fix is emphatically
not "pad in every string function"; that list is measured, and `length` sitting
outside it while `octet_length` sits inside is exactly the kind of split a
guess gets wrong.

**Pattern matching was returning wrong rows.** `LIKE` / `ILIKE` / `SIMILAR TO`
/ `~` are NOT blank-insensitive the way `=` is — they match the padded output —
so a `char(5)` holding `'ab'` does not match `LIKE 'ab'`. SecantusDB matched
the unpadded value and returned a row PostgreSQL excludes, in the WHERE clause
as well as the select list. The pattern cannot be adjusted instead: `'ab   '`
matches `'ab%'` but not `'ab_'`, so no rewrite of the pattern reproduces the
rule for every shape.

Also fixed: a cast of a NON-string to `char(n)` skipped the length limit
entirely (`123::char(2)` was `'123'`), because a `char(n)` target's type tag is
plain `text` and the number-to-text branch returns before the body's own
char-length block ever runs.

Every expectation here was measured against PostgreSQL 14.13.
"""

from __future__ import annotations

import pytest

from secantus.sql.pgserver import SecantusPGServer
from secantus.storage import Storage

psycopg = pytest.importorskip("psycopg")


@pytest.fixture
def conn(tmp_path):
    st = Storage(str(tmp_path / "s13"))
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
    conn.execute("CREATE TABLE c16 (id int PRIMARY KEY, c char(5), v varchar(5), t text)")
    conn.execute("INSERT INTO c16 VALUES (1,'ab','ab','ab'),(2,'abcde','abcde','abcde')")
    return conn


def one(c, sql):
    return c.execute(sql).fetchone()[0]


# --- the cast path: truncate, and let the wire do the padding ---------------- #


@pytest.mark.parametrize(
    "sql,expected",
    [
        # A bare cast still reaches the client PADDED — the wire pads it.
        ("SELECT 'a'::char(3)", "a  "),
        ("SELECT 'abc'::char(2)", "ab"),
        # ...but every conversion to text strips those blanks again.
        ("SELECT 'a'::char(3) || '|'", "a|"),
        ("SELECT ('a'::char(3))::text || '|'", "a|"),
        ("SELECT ('a'::char(3))::varchar || '|'", "a|"),
        ("SELECT upper('a'::char(3)) || '|'", "A|"),
        ("SELECT substr('a'::char(3), 1, 3) || '|'", "a|"),
        ("SELECT 'x' || 'a'::char(3) || 'y'", "xay"),
        # A NON-string source is rendered to text and THEN truncated.
        ("SELECT 123::char(2)", "12"),
        ("SELECT 1.50::char(6) || '|'", "1.50|"),
        ("SELECT true::char(2) || '|'", "tr|"),
    ],
)
def test_char_cast(conn, sql, expected):
    assert one(conn, sql) == expected


def test_char_cast_length_is_measured_in_characters(conn):
    assert one(conn, "SELECT length('a'::char(3))") == 1
    assert one(conn, "SELECT char_length('a'::char(3))") == 1


def test_char_cast_comparison_ignores_trailing_blanks(conn):
    assert one(conn, "SELECT 'a'::char(3) = 'a'") is True


# --- the column path --------------------------------------------------------- #


def test_column_reaches_the_client_padded(seeded):
    assert one(seeded, "SELECT c FROM c16 WHERE id=1") == "ab   "


@pytest.mark.parametrize(
    "sql,expected",
    [
        # Text conversions strip.
        ("SELECT c || '|' FROM c16 WHERE id=1", "ab|"),
        ("SELECT upper(c)||'|' FROM c16 WHERE id=1", "AB|"),
        ("SELECT left(c,4)||'|' FROM c16 WHERE id=1", "ab|"),
        ("SELECT c::varchar||'|' FROM c16 WHERE id=1", "ab|"),
        # char(5) -> char(9) strips first, then re-pads to 9, then `||` strips.
        ("SELECT c::char(9)||'|' FROM c16 WHERE id=1", "ab|"),
        ("SELECT md5(c) FROM c16 WHERE id=1", "187ef4436122d1cc2f40dc2b92f0eba0"),
        # The output-form functions see the PADDED value.
        ("SELECT concat(c,'|') FROM c16 WHERE id=1", "ab   |"),
        ("SELECT concat_ws('-',c,'x') FROM c16 WHERE id=1", "ab   -x"),
        ("SELECT format('%s|', c) FROM c16 WHERE id=1", "ab   |"),
        ("SELECT to_json(c)::text FROM c16 WHERE id=1", '"ab   "'),
    ],
)
def test_column_text_versus_output_form(seeded, sql, expected):
    assert one(seeded, sql) == expected


def test_length_strips_but_octet_length_does_not(seeded):
    """The split that a guess gets wrong: same value, same row, different rule."""
    assert one(seeded, "SELECT length(c) FROM c16 WHERE id=1") == 2
    assert one(seeded, "SELECT char_length(c) FROM c16 WHERE id=1") == 2
    assert one(seeded, "SELECT octet_length(c) FROM c16 WHERE id=1") == 5


def test_cast_to_bytea_encodes_the_padded_bytes(seeded):
    assert one(seeded, "SELECT c::bytea FROM c16 WHERE id=1") == b"ab   "


# --- pattern matching is NOT blank-insensitive ------------------------------- #


@pytest.mark.parametrize(
    "sql,expected",
    [
        # `=` ignores trailing blanks on both sides...
        ("SELECT c = 'ab' FROM c16 WHERE id=1", True),
        ("SELECT c = 'ab   ' FROM c16 WHERE id=1", True),
        # ...but LIKE and friends match the PADDED value, so these differ.
        ("SELECT c LIKE 'ab' FROM c16 WHERE id=1", False),
        ("SELECT c LIKE 'ab   ' FROM c16 WHERE id=1", True),
        ("SELECT c NOT LIKE 'ab' FROM c16 WHERE id=1", True),
        ("SELECT c ILIKE 'AB   ' FROM c16 WHERE id=1", True),
        ("SELECT c ~ '^ab$' FROM c16 WHERE id=1", False),
        ("SELECT c SIMILAR TO 'ab' FROM c16 WHERE id=1", False),
        # `%` still spans the padding; `_` counts it. This pair is why the
        # PATTERN cannot be rewritten instead of padding the value.
        ("SELECT c LIKE 'ab%' FROM c16 WHERE id=1", True),
        ("SELECT c LIKE 'ab_' FROM c16 WHERE id=1", False),
        # varchar and text are blank-SENSITIVE and unaffected by any of this.
        ("SELECT v LIKE 'ab' FROM c16 WHERE id=1", True),
        ("SELECT t LIKE 'ab' FROM c16 WHERE id=1", True),
    ],
)
def test_pattern_match_sees_the_padding(seeded, sql, expected):
    assert one(seeded, sql) is expected


def test_where_clause_agrees_with_the_select_list(seeded):
    """The pushdown path is separate from the scalar path — it had the same bug,
    which is the shape that returns WRONG ROWS rather than a wrong column."""
    assert seeded.execute("SELECT id FROM c16 WHERE c LIKE 'ab' ORDER BY id").fetchall() == []
    assert seeded.execute("SELECT id FROM c16 WHERE c LIKE 'ab   ' ORDER BY id").fetchall() == [
        (1,)
    ]
    assert seeded.execute("SELECT id FROM c16 WHERE c LIKE 'ab%' ORDER BY id").fetchall() == [
        (1,),
        (2,),
    ]
    # varchar unchanged.
    assert seeded.execute("SELECT id FROM c16 WHERE v LIKE 'ab' ORDER BY id").fetchall() == [(1,)]


# --- a record FIELD renders through the output function ---------------------- #


@pytest.mark.parametrize(
    "sql",
    [
        "SELECT ('a'::text, 'd'::char(2))",
        "SELECT ROW('a'::text, 'd'::char(2))",
    ],
)
def test_char_field_of_a_record_keeps_its_padding(conn, sql):
    """The one place a `char(n)` CAST keeps its blanks: as a composite field,
    which renders with the field type's output function rather than as text.

    Asserted on the FIELD rather than on `record::text`, because that cast
    still renders a record as JSON instead of PostgreSQL's `(a,"d ")` — a
    separate bug, recorded in `tasks/backlog.md`."""
    assert "d " in one(conn, sql)
