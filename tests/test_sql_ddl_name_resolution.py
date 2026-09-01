"""DDL relation naming: which schema a statement resolves, and what PG calls
the relation when it refuses.

Found 2026-09-01 by running each shape against the live PostgreSQL 14.13 on
this box rather than reading the code. Five defects, in descending severity:

1. **``ALTER TABLE`` ignored the schema entirely.** ``plan_alter_table`` took
   ``stmt.this.name`` — the bare name — throwing away the qualifier that
   ``qualify_from_search_path`` had just resolved. Every ALTER form (ADD /
   DROP / RENAME COLUMN, RENAME TO, ALTER COLUMN, ADD CONSTRAINT, ADD PRIMARY
   KEY) answered 42P01 for any table outside ``public``, whether reached
   through ``search_path`` or written out as ``schema.table``.
2. **``ALTER TABLE … RENAME TO`` would have moved the table to ``public``**,
   because the new catalog key was the bare name (latent behind 1).
3. **``DROP SCHEMA … CASCADE`` left views, matviews and sequences behind**,
   and a bare ``DROP SCHEMA`` did not count them as dependants — so they
   outlived their schema and then collided with a later ``CREATE``.
4. **A bare ``CREATE SEQUENCE`` ignored ``search_path``** and always created
   in ``public``, so a later ``CREATE SEQUENCE schema.s`` saw a free name and
   silently created a second sequence where PG raises 42P07.
5. **Error messages named the resolved key, not the relation the user wrote**
   (``sa.onlypub`` for a written ``onlypub``), used ``relation`` where PG uses
   the DROP verb's noun, and answered "does not exist" for a name taken by a
   different relation KIND (PG: ``42809 "v" is not a table``).
"""

from __future__ import annotations

import pytest

import pg_oracle
from secantus.sql.engine import run_sql
from secantus.sql.errors import SQLError
from secantus.sql.session import Session
from secantus.storage import Storage


@pytest.fixture()
def db(tmp_path):
    storage = Storage(str(tmp_path))
    session = Session(database="t")

    def run(sql: str):
        return [r.rows for r in run_sql(storage, "t", sql, session=session)][0]

    run.session = session  # type: ignore[attr-defined]
    try:
        yield run
    finally:
        storage.close()


def _err(db, sql) -> tuple[str, str]:
    with pytest.raises(SQLError) as ei:
        db(sql)
    return ei.value.sqlstate, str(ei.value)


# --------------------------------------------------------------------------- #
# 1 + 2 — ALTER TABLE resolves its target
# --------------------------------------------------------------------------- #


ALTER_SHAPES = [
    "ALTER TABLE {t} ADD COLUMN y int",
    "ALTER TABLE {t} DROP COLUMN x",
    "ALTER TABLE {t} RENAME COLUMN x TO y",
    "ALTER TABLE {t} ALTER COLUMN x SET NOT NULL",
    "ALTER TABLE {t} ADD CONSTRAINT ck CHECK (x > 0)",
    "ALTER TABLE {t} ADD PRIMARY KEY (x)",
    "ALTER TABLE {t} RENAME TO renamed",
]


class TestAlterResolvesItsTarget:
    @pytest.mark.parametrize("shape", ALTER_SHAPES)
    def test_bare_name_through_search_path(self, db, shape):
        db("CREATE SCHEMA s1")
        db("CREATE TABLE s1.a (x int)")
        db("SET search_path TO s1, public")
        db(shape.format(t="a"))  # must not raise

    @pytest.mark.parametrize("shape", ALTER_SHAPES)
    def test_explicit_schema_qualifier(self, db, shape):
        db("CREATE SCHEMA s1")
        db("CREATE TABLE s1.a (x int)")
        db(shape.format(t="s1.a"))  # must not raise

    def test_rename_stays_in_its_schema(self, db):
        db("CREATE SCHEMA s1")
        db("CREATE TABLE s1.a (x int)")
        db("INSERT INTO s1.a VALUES (7)")
        db("ALTER TABLE s1.a RENAME TO b")
        assert db("SELECT x FROM s1.b") == [(7,)]
        # and did NOT leak into public
        assert _err(db, "SELECT x FROM public.b")[0] == "42P01"

    def test_rename_rejects_a_qualified_target(self, db):
        """PG: `RENAME TO s.t` is a syntax error — the new name is always
        interpreted in the relation's own schema."""
        db("CREATE SCHEMA s1")
        db("CREATE TABLE s1.a (x int)")
        assert _err(db, "ALTER TABLE s1.a RENAME TO s1.b")[0] == "42601"

    def test_rename_onto_an_existing_name(self, db):
        db("CREATE SCHEMA s1")
        db("CREATE TABLE s1.a (x int)")
        db("CREATE TABLE s1.b (x int)")
        state, msg = _err(db, "ALTER TABLE s1.a RENAME TO b")
        assert state == "42P07"
        assert msg == 'relation "b" already exists'

    def test_rename_to_its_own_name(self, db):
        db("CREATE SCHEMA s1")
        db("CREATE TABLE s1.a (x int)")
        assert _err(db, "ALTER TABLE s1.a RENAME TO a")[0] == "42P07"


# --------------------------------------------------------------------------- #
# 3 — DROP SCHEMA accounts for every relation kind
# --------------------------------------------------------------------------- #


class TestDropSchemaAccounting:
    def test_cascade_drops_views_and_sequences(self, db):
        db("CREATE SCHEMA s1")
        db("CREATE TABLE s1.t (x int)")
        db("CREATE VIEW s1.v AS SELECT 1 AS x")
        db("CREATE SEQUENCE s1.sq")
        db("DROP SCHEMA s1 CASCADE")
        # Re-creating the schema must give a clean slate — before the fix the
        # view and the sequence survived and collided here.
        db("CREATE SCHEMA s1")
        db("CREATE VIEW s1.v AS SELECT 1 AS x")
        db("CREATE SEQUENCE s1.sq")

    @pytest.mark.parametrize("obj", ["CREATE VIEW s1.v AS SELECT 1 AS x", "CREATE SEQUENCE s1.sq"])
    def test_without_cascade_a_view_or_sequence_blocks_the_drop(self, db, obj):
        db("CREATE SCHEMA s1")
        db(obj)
        assert _err(db, "DROP SCHEMA s1")[0] == "2BP01"


# --------------------------------------------------------------------------- #
# 4 — CREATE SEQUENCE is homed by search_path
# --------------------------------------------------------------------------- #


class TestCreateSequenceHoming:
    def test_bare_create_lands_in_the_paths_first_schema(self, db):
        db("CREATE SCHEMA s1")
        db("SET search_path TO s1, public")
        db("CREATE SEQUENCE sq")
        # The qualified name is now taken — PG agrees.
        assert _err(db, "CREATE SEQUENCE s1.sq")[0] == "42P07"

    def test_duplicate_names_the_bare_relation(self, db):
        db("CREATE SCHEMA s1")
        db("CREATE SEQUENCE s1.sq")
        assert _err(db, "CREATE SEQUENCE s1.sq")[1] == 'relation "sq" already exists'


# --------------------------------------------------------------------------- #
# 5 — error naming: as-written relation, per-kind noun, wrong-kind 42809
# --------------------------------------------------------------------------- #


class TestErrorNaming:
    @pytest.mark.parametrize(
        "sql",
        [
            "SELECT * FROM onlypub",
            "INSERT INTO onlypub VALUES (1)",
            "UPDATE onlypub SET a = 1",
            "DELETE FROM onlypub",
            "ALTER TABLE onlypub ADD COLUMN z int",
        ],
    )
    def test_names_the_relation_the_user_wrote(self, db, sql):
        """With `public` off the path the relation is invisible, and PG names
        it exactly as written — not `sa.onlypub`, a schema the user never
        typed."""
        db("CREATE SCHEMA sa")
        db("CREATE TABLE onlypub (a int)")
        db("SET search_path TO sa")
        state, msg = _err(db, sql)
        assert state == "42P01"
        assert msg == 'relation "onlypub" does not exist'

    def test_keeps_an_explicit_public_qualifier(self, db):
        assert _err(db, "SELECT * FROM public.nope")[1] == 'relation "public.nope" does not exist'

    @pytest.mark.parametrize(
        ("sql", "state", "msg"),
        [
            ("DROP TABLE nope", "42P01", 'table "nope" does not exist'),
            ("DROP VIEW nope", "42P01", 'view "nope" does not exist'),
            ("DROP SEQUENCE nope", "42P01", 'sequence "nope" does not exist'),
            ("DROP INDEX nope", "42704", 'index "nope" does not exist'),
        ],
    )
    def test_missing_relation_uses_the_drop_verbs_noun(self, db, sql, state, msg):
        assert _err(db, sql) == (state, msg)

    @pytest.mark.parametrize(
        ("sql", "msg"),
        [
            ("DROP TABLE v", '"v" is not a table'),
            ("DROP SEQUENCE v", '"v" is not a sequence'),
            ("DROP INDEX v", '"v" is not an index'),
            ("DROP VIEW t", '"t" is not a view'),
            ("DROP TABLE sq", '"sq" is not a table'),
        ],
    )
    def test_wrong_kind_is_42809_not_does_not_exist(self, db, sql, msg):
        """A name taken by another relation KIND is 42809, not 42P01 — and
        `IF EXISTS` does not suppress it, because the object is present."""
        db("CREATE TABLE t (x int)")
        db("CREATE VIEW v AS SELECT 1 AS x")
        db("CREATE SEQUENCE sq")
        assert _err(db, sql) == ("42809", msg)

    def test_duplicate_table_names_the_bare_relation(self, db):
        db("CREATE SCHEMA s1")
        db("CREATE TABLE s1.t (x int)")
        assert _err(db, "CREATE TABLE s1.t (x int)")[1] == 'relation "t" already exists'


# --------------------------------------------------------------------------- #
# Follow-ups the same probe run turned up
# --------------------------------------------------------------------------- #


class TestMatviewsAreSchemaAware:
    """Every matview path read the bare `stmt.this.name`, so a matview was
    created, catalogued and stored unqualified whatever schema the statement
    named — `SELECT … FROM s.mv` was 42P01, and two schemas could not hold
    same-named matviews."""

    def test_created_in_the_paths_first_schema(self, db):
        db("CREATE SCHEMA s1")
        db("CREATE TABLE s1.b (x int)")
        db("INSERT INTO s1.b VALUES (1), (2)")
        db("SET search_path TO s1, public")
        db("CREATE MATERIALIZED VIEW mv AS SELECT x FROM s1.b")
        assert db("SELECT count(*) FROM s1.mv") == [(2,)]

    def test_two_schemas_can_hold_the_same_name(self, db):
        db("CREATE SCHEMA s1")
        db("CREATE SCHEMA s2")
        db("CREATE MATERIALIZED VIEW s1.mv AS SELECT 1 AS x")
        db("CREATE MATERIALIZED VIEW s2.mv AS SELECT 2 AS x")
        assert db("SELECT x FROM s1.mv") == [(1,)]
        assert db("SELECT x FROM s2.mv") == [(2,)]

    def test_refresh_and_drop_reach_a_qualified_matview(self, db):
        db("CREATE SCHEMA s1")
        db("CREATE TABLE s1.b (x int)")
        db("INSERT INTO s1.b VALUES (1)")
        db("CREATE MATERIALIZED VIEW s1.mv AS SELECT x FROM s1.b")
        db("INSERT INTO s1.b VALUES (2)")
        db("REFRESH MATERIALIZED VIEW s1.mv")
        assert db("SELECT count(*) FROM s1.mv") == [(2,)]
        db("DROP MATERIALIZED VIEW s1.mv")
        assert _err(db, "SELECT x FROM s1.mv")[0] == "42P01"

    def test_drop_verbs_do_not_alias(self, db):
        """PG keeps VIEW and MATERIALIZED VIEW strictly distinct."""
        db("CREATE MATERIALIZED VIEW mv AS SELECT 1 AS x")
        db("CREATE VIEW v AS SELECT 1 AS x")
        assert _err(db, "DROP VIEW mv") == ("42809", '"mv" is not a view')
        assert _err(db, "DROP MATERIALIZED VIEW v") == (
            "42809",
            '"v" is not a materialized view',
        )


class TestSequenceAsARelation:
    """PG exposes a sequence as a one-row relation; ours was not reachable from
    the FROM clause at all."""

    def test_fresh_sequence(self, db):
        db("CREATE SEQUENCE s")
        assert db("SELECT last_value, is_called FROM s") == [(1, False)]

    def test_tracks_the_value_actually_handed_out(self, db):
        """The persisted `last_value` is the pre-allocated batch's high-water
        mark, so reading the doc directly reported a position far ahead of the
        truth (128 after two `nextval` at the default batch)."""
        db("CREATE SEQUENCE s")
        db("SELECT nextval('s')")
        db("SELECT nextval('s')")
        assert db("SELECT last_value, is_called FROM s") == [(2, True)]

    def test_resolves_through_search_path(self, db):
        db("CREATE SCHEMA s1")
        db("CREATE SEQUENCE s1.sq")
        db("SET search_path TO s1, public")
        assert db("SELECT last_value FROM sq") == [(1,)]

    def test_projection_and_where_apply(self, db):
        db("CREATE SEQUENCE s")
        db("SELECT nextval('s')")
        assert db("SELECT is_called FROM s WHERE last_value >= 1") == [(True,)]


class TestSelectInto:
    """`SELECT … INTO t` is PG's older spelling of `CREATE TABLE t AS SELECT`.
    It was not dispatched at all, so the INTO target was resolved as a SOURCE
    relation and every such statement failed with 42P01."""

    def test_creates_and_populates(self, db):
        db("CREATE TABLE b (x int)")
        db("INSERT INTO b VALUES (1), (2)")
        res = db("SELECT x INTO t FROM b")
        assert res == []
        assert db("SELECT x FROM t ORDER BY x") == [(1,), (2,)]

    def test_qualified_target(self, db):
        db("CREATE SCHEMA s1")
        db("CREATE TABLE b (x int)")
        db("INSERT INTO b VALUES (7)")
        db("SELECT x INTO s1.t FROM b")
        assert db("SELECT x FROM s1.t") == [(7,)]

    def test_target_is_a_definition_not_a_reference(self, db):
        """With `s1.t` already present and `s1` on the path, the INTO target
        must not bind to it — PG reports the name as already taken."""
        db("CREATE SCHEMA s1")
        db("CREATE TABLE s1.t (x int)")
        db("CREATE TABLE b (x int)")
        db("SET search_path TO s1, public")
        assert _err(db, "SELECT x INTO t FROM b")[0] == "42P07"


# --------------------------------------------------------------------------- #
# The reference server has the last word.
# --------------------------------------------------------------------------- #

#: (setup, probe) pairs. Every probe below diverged from PostgreSQL before
#: 2026-09-01; the assertion is an exact match on SQLSTATE and message.
_ORACLE_SHAPES = [
    "ALTER TABLE oz.a ADD COLUMN y int",
    "ALTER TABLE oz.a RENAME TO b",
    "ALTER TABLE oz.a RENAME TO oz.b",
    "ALTER TABLE nosuch_rel ADD COLUMN y int",
    "DROP TABLE nosuch_rel",
    "DROP VIEW nosuch_rel",
    "DROP SEQUENCE nosuch_rel",
    "DROP INDEX nosuch_rel",
    "DROP TABLE oz.v",
    "DROP VIEW oz.a",
    "DROP SEQUENCE oz.a",
    "DROP INDEX oz.a",
    "CREATE TABLE oz.a (x int)",
    "CREATE VIEW oz.v AS SELECT 1 AS x",
    "CREATE SEQUENCE oz.sq",
    "SELECT * FROM public.nosuch_rel",
    "SELECT * FROM oz.nosuch_rel",
    "CREATE MATERIALIZED VIEW oz.mv2 AS SELECT 1 AS x",
    "DROP VIEW oz.mv",
    "DROP MATERIALIZED VIEW oz.v",
    "DROP MATERIALIZED VIEW nosuch_rel",
    "SELECT last_value, is_called FROM oz.sq",
    "SELECT x INTO oz.si FROM oz.a",
]


@pytest.mark.skipif(not pg_oracle.available(), reason=pg_oracle.skip_reason())
def test_ddl_naming_matches_real_postgres(db):
    """The classes above say what we believe; this says what PostgreSQL does.

    Every shape is run against both servers and the SQLSTATE *and* the message
    text must agree — the message is the whole point of most of these fixes.
    """
    setup = [
        "DROP SCHEMA IF EXISTS oz CASCADE",
        "CREATE SCHEMA oz",
        "CREATE TABLE oz.a (x int)",
        "CREATE VIEW oz.v AS SELECT 1 AS x",
        "CREATE MATERIALIZED VIEW oz.mv AS SELECT 1 AS x",
        "CREATE SEQUENCE oz.sq",
    ]
    pg = pg_oracle.connect()
    assert pg is not None
    pg.autocommit = True

    def theirs(sql):
        try:
            pg.execute(sql)
            return ("OK", "")
        except Exception as exc:  # noqa: BLE001
            # `message_primary`, not `str(exc)`: psycopg appends the statement
            # context ("LINE 1: …" and a caret) to the rendered string, which
            # is client-side formatting rather than part of what the server
            # said. The primary message is the field the wire carries.
            diag = getattr(exc, "diag", None)
            primary = getattr(diag, "message_primary", None) or str(exc).strip()
            return (getattr(diag, "sqlstate", None), primary)

    def ours(sql):
        try:
            db(sql)
            return ("OK", "")
        except SQLError as exc:
            return (exc.sqlstate, str(exc).strip())

    try:
        for stmt in setup:
            theirs(stmt)
            ours(stmt)
        for shape in _ORACLE_SHAPES:
            # Each probe runs on a freshly re-created `oz`, so an earlier
            # mutating probe cannot change a later one's answer.
            for stmt in setup:
                theirs(stmt)
                ours(stmt)
            assert ours(shape) == theirs(shape), shape
    finally:
        pg.execute("DROP SCHEMA IF EXISTS oz CASCADE")
        pg.close()
