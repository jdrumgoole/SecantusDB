"""P2 tests: session functions, SHOW/SET, and catalog virtual tables.

Driven through ``run_sql`` with an explicit ``Session`` (the embedded view);
the wire-level coverage lives in ``test_pgserver.py``.
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB, user="joe", backend_pid=4242)


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[0]


# -- session / info functions ------------------------------------------------ #


def test_version(storage, session):
    res = q(storage, session, "SELECT version()")
    assert res.columns[0].name == "version"
    assert res.rows[0][0].startswith("PostgreSQL 15.0 (SecantusDB)")


def test_schema_qualified_function(storage, session):
    # SQLAlchemy's init calls pg_catalog.version() — the catalog qualifier is
    # stripped and the function evaluated.
    res = q(storage, session, "SELECT pg_catalog.version()")
    assert res.rows[0][0].startswith("PostgreSQL 15.0 (SecantusDB)")


def test_current_database_user_schema(storage, session):
    assert q(storage, session, "SELECT current_database()").rows == [("testdb",)]
    assert q(storage, session, "SELECT current_user").rows == [("joe",)]
    assert q(storage, session, "SELECT current_schema()").rows == [("public",)]


def test_pg_backend_pid_is_int4(storage, session):
    res = q(storage, session, "SELECT pg_backend_pid()")
    assert res.rows == [(4242,)]
    assert res.columns[0].type_tag == "int4"


def test_select_alias_names_column(storage, session):
    res = q(storage, session, "SELECT current_database() AS db")
    assert res.columns[0].name == "db"


# -- SET / SHOW / RESET ------------------------------------------------------ #


def test_set_show_reset_roundtrip(storage, session):
    assert q(storage, session, "SET search_path TO myschema").command_tag == "SET"
    assert q(storage, session, "SHOW search_path").rows == [("myschema",)]
    assert q(storage, session, "SELECT current_setting('search_path')").rows == [("myschema",)]
    assert q(storage, session, "RESET search_path").command_tag == "RESET"
    # Back to the default after RESET.
    assert q(storage, session, "SHOW search_path").rows == [('"$user", public',)]


def test_set_reportable_guc_surfaces_parameter_status(storage, session):
    res = q(storage, session, "SET client_encoding = 'LATIN1'")
    assert ("client_encoding", "LATIN1") in res.parameter_status


def test_transaction_control_is_accepted(storage, session):
    assert q(storage, session, "BEGIN").command_tag == "BEGIN"
    assert q(storage, session, "COMMIT").command_tag == "COMMIT"
    assert q(storage, session, "ROLLBACK").command_tag == "ROLLBACK"


# -- catalog virtual tables -------------------------------------------------- #


def _seed(storage, session):
    q(storage, session, "CREATE TABLE users (id bigint primary key, name text, age int not null)")
    q(storage, session, "CREATE TABLE orders (id bigint primary key, total numeric)")


def test_information_schema_tables(storage, session):
    _seed(storage, session)
    res = q(
        storage,
        session,
        "SELECT table_name, table_type FROM information_schema.tables "
        "WHERE table_schema = 'public' ORDER BY table_name",
    )
    assert res.rows == [("orders", "BASE TABLE"), ("users", "BASE TABLE")]


def test_information_schema_columns(storage, session):
    _seed(storage, session)
    res = q(
        storage,
        session,
        "SELECT column_name, data_type, is_nullable FROM information_schema.columns "
        "WHERE table_name = 'users' ORDER BY ordinal_position",
    )
    assert res.rows == [
        ("id", "bigint", "NO"),
        ("name", "text", "YES"),
        ("age", "integer", "NO"),
    ]


def test_pg_class_and_namespace(storage, session):
    _seed(storage, session)
    assert q(
        storage,
        session,
        "SELECT relname FROM pg_catalog.pg_class WHERE relkind = 'r' ORDER BY relname",
    ).rows == [
        ("orders",),
        ("users",),
    ]
    names = {r[0] for r in q(storage, session, "SELECT nspname FROM pg_catalog.pg_namespace").rows}
    assert {"pg_catalog", "public", "information_schema"} <= names


def test_pg_type_lists_known_oids(storage, session):
    res = q(storage, session, "SELECT typname FROM pg_catalog.pg_type WHERE typname = 'int8'")
    assert res.rows == [("int8",)]


def test_count_star_over_virtual_table(storage, session):
    _seed(storage, session)
    assert q(storage, session, "SELECT COUNT(*) FROM information_schema.tables").rows == [(2,)]


def test_catalog_join_class_namespace(storage, session):
    # The join interactive psql's \d emits: pg_class ⋈ pg_namespace on the
    # namespace oid. Every user table lives in ``public`` (relnamespace 2200).
    _seed(storage, session)
    res = q(
        storage,
        session,
        "SELECT c.relname, n.nspname FROM pg_catalog.pg_class c "
        "JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace "
        "WHERE c.relkind = 'r' ORDER BY c.relname",
    )
    assert res.rows == [("orders", "public"), ("users", "public")]


def test_catalog_join_with_where_on_namespace(storage, session):
    # Filtering by the joined namespace name restricts to public's relations.
    _seed(storage, session)
    res = q(
        storage,
        session,
        "SELECT c.relname FROM pg_catalog.pg_class c "
        "JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace "
        "WHERE n.nspname = 'public' AND c.relkind = 'r' ORDER BY c.relname",
    )
    assert res.rows == [("orders",), ("users",)]


def test_pg_attribute_lists_columns(storage, session):
    _seed(storage, session)
    res = q(
        storage,
        session,
        "SELECT attname, atttypid, attnotnull FROM pg_catalog.pg_attribute a "
        "JOIN pg_catalog.pg_class c ON a.attrelid = c.oid "
        "WHERE c.relname = 'users' ORDER BY a.attnum",
    )
    # id bigint PK (oid 20, NOT NULL), name text (25, nullable), age int (23, NOT NULL).
    assert res.rows == [("id", 20, True), ("name", 25, False), ("age", 23, True)]


def test_pg_attribute_three_way_join(storage, session):
    _seed(storage, session)
    res = q(
        storage,
        session,
        "SELECT a.attname FROM pg_catalog.pg_attribute a "
        "JOIN pg_catalog.pg_class c ON a.attrelid = c.oid "
        "JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace "
        "WHERE n.nspname = 'public' AND c.relname = 'orders' ORDER BY a.attnum",
    )
    assert res.rows == [("id",), ("total",)]


def test_pg_index_and_constraint_populated(storage, session):
    # A declared table with a PK has an implicit PK index relation + a 'p'
    # constraint; a user CREATE INDEX adds another (non-primary) index.
    _seed(storage, session)
    q(storage, session, "CREATE INDEX ix_age ON users (age)")
    idx = q(
        storage,
        session,
        "SELECT i.indisprimary, i.indisunique FROM pg_catalog.pg_index i "
        "JOIN pg_catalog.pg_class c ON i.indexrelid = c.oid "
        "JOIN pg_catalog.pg_class t ON i.indrelid = t.oid "
        "WHERE t.relname = 'users' ORDER BY i.indisprimary DESC",
    )
    assert (True, True) in idx.rows  # the PK index (primary + unique)
    assert (False, False) in idx.rows  # the user index on age
    # The PK surfaces as a contype 'p' constraint.
    pk = q(
        storage,
        session,
        "SELECT con.conname FROM pg_catalog.pg_constraint con "
        "JOIN pg_catalog.pg_class t ON con.conrelid = t.oid "
        "WHERE t.relname = 'users' AND con.contype = 'p'",
    )
    assert pk.rows == [("users_pkey",)]


def test_unnest_expands_index_key_array(storage, session):
    # unnest(indkey) + generate_subscripts expand the index key array into one
    # row per column with its 1-based ordinal — the core of PK/index reflection.
    _seed(storage, session)
    res = q(
        storage,
        session,
        "SELECT unnest(i.indkey) AS attnum, generate_subscripts(i.indkey, 1) AS ord "
        "FROM pg_catalog.pg_index i JOIN pg_catalog.pg_class c ON i.indexrelid = c.oid "
        "WHERE c.relname = 'users_pkey'",
    )
    # users PK is on a single column (id → attnum 1, ordinal 1).
    assert res.rows == [(1, 1)]


def test_group_over_derived_table_with_array_agg(storage, session):
    # GROUP BY over a (SELECT ...) AS x derived table, collecting with array_agg —
    # the shape SQLAlchemy's get_pk_constraint uses.
    _seed(storage, session)
    res = q(
        storage,
        session,
        "SELECT x.relname, array_agg(x.attname) AS cols FROM ("
        "SELECT c.relname AS relname, a.attname AS attname "
        "FROM pg_catalog.pg_attribute a "
        "JOIN pg_catalog.pg_class c ON a.attrelid = c.oid "
        "WHERE c.relname = 'users') AS x GROUP BY x.relname",
    )
    assert res.rows[0][0] == "users"
    assert sorted(res.rows[0][1]) == ["age", "id", "name"]


def test_pg_attrdef_and_description_empty(storage, session):
    _seed(storage, session)
    assert q(storage, session, "SELECT * FROM pg_catalog.pg_attrdef").rows == []
    assert q(storage, session, "SELECT * FROM pg_catalog.pg_description").rows == []


def test_format_type_in_join_projection(storage, session):
    # A scalar catalog function (format_type) in the SELECT list of a join —
    # evaluated per row in Python; maps the type OID to its SQL spelling.
    _seed(storage, session)
    res = q(
        storage,
        session,
        "SELECT a.attname, format_type(a.atttypid, a.atttypmod) AS t "
        "FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_class c ON a.attrelid = c.oid "
        "WHERE c.relname = 'users' ORDER BY a.attnum",
    )
    assert res.rows == [("id", "bigint"), ("name", "text"), ("age", "integer")]


def test_compound_on_multikey_join(storage, session):
    # pg_attribute ⋈ pg_description on TWO equality keys (objoid=attrelid AND
    # objsubid=attnum). pg_description is empty, so a LEFT JOIN yields NULL.
    _seed(storage, session)
    res = q(
        storage,
        session,
        "SELECT a.attname, d.description FROM pg_catalog.pg_attribute a "
        "JOIN pg_catalog.pg_class c ON a.attrelid = c.oid "
        "LEFT OUTER JOIN pg_catalog.pg_description d "
        "ON d.objoid = a.attrelid AND d.objsubid = a.attnum "
        "WHERE c.relname = 'users' ORDER BY a.attnum",
    )
    assert res.rows == [("id", None), ("name", None), ("age", None)]


def test_residual_on_predicate(storage, session):
    # A compound ON with a residual filter on the joined table (attnum > 0) —
    # folded into the $lookup sub-pipeline.
    _seed(storage, session)
    res = q(
        storage,
        session,
        "SELECT a.attname FROM pg_catalog.pg_class c "
        "LEFT OUTER JOIN pg_catalog.pg_attribute a "
        "ON c.oid = a.attrelid AND a.attnum > 0 AND NOT a.attisdropped "
        "WHERE c.relname = 'users' ORDER BY a.attnum",
    )
    assert res.rows == [("id",), ("name",), ("age",)]


def test_case_and_correlated_subquery_in_projection(storage, session):
    # CASE + a correlated scalar subquery (over the empty pg_attrdef) — both
    # evaluated per row; default has no rows so the subquery is NULL.
    _seed(storage, session)
    res = q(
        storage,
        session,
        "SELECT a.attname, "
        "(SELECT d.adbin FROM pg_catalog.pg_attrdef d "
        " WHERE d.adrelid = a.attrelid AND d.adnum = a.attnum) AS deflt, "
        "CASE WHEN a.attnotnull THEN 'NN' ELSE 'null' END AS nn "
        "FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_class c ON a.attrelid = c.oid "
        "WHERE c.relname = 'users' ORDER BY a.attnum",
    )
    assert res.rows == [("id", None, "NN"), ("name", None, "null"), ("age", None, "NN")]


def test_residual_on_with_text_bound_int(storage, session):
    # Regression: a residual ON predicate comparing a numeric column to a value
    # that arrives as text (extended-protocol bind) must compare numerically, via
    # the CAST's target type — not as a string (Mongo orders numbers < strings),
    # else the join would silently drop every row.
    from secantus.sql import planner
    from secantus.sql.engine import run_statement

    _seed(storage, session)
    stmt = planner.parse(
        "SELECT a.attname FROM pg_catalog.pg_class c "
        "LEFT OUTER JOIN pg_catalog.pg_attribute a "
        "ON c.oid = a.attrelid AND a.attnum > CAST($1 AS SMALLINT) AND NOT a.attisdropped "
        "WHERE c.relname = 'users' ORDER BY a.attnum"
    )[0]
    bound = planner.substitute_parameters(stmt, ["0"])  # text-bound, as the wire does
    out = run_statement(storage, DB, bound, session)
    assert out.rows == [("id",), ("name",), ("age",)]


def test_group_by_over_virtual_table(storage, session):
    # GROUP BY over a virtual catalog table goes through the aggregation pipeline
    # backed by CatalogBackend — count columns for a given base table.
    _seed(storage, session)
    res = q(
        storage,
        session,
        "SELECT c.table_name, COUNT(*) AS n "
        "FROM information_schema.columns c "
        "WHERE c.table_name = 'users' GROUP BY c.table_name",
    )
    assert res.rows == [("users", 3)]


def test_pg_type_typarray_and_pg_range(storage, session):
    rows = run_sql(
        storage,
        "db",
        "select typname, typarray, typdelim from pg_type where typname = 'int4'",
        session=session,
    )[-1].rows
    assert rows == [("int4", 1007, ",")]
    rows = run_sql(
        storage,
        "db",
        "select rngsubtype, rngmultitypid from pg_range where rngtypid = 3904",
        session=session,
    )[-1].rows
    assert rows == [(23, 4451)]


def test_to_regtype_and_regtype_cast(storage, session):
    def rows(sql):
        return run_sql(storage, "db", sql, session=session)[-1].rows

    assert rows("select to_regtype('int4'), to_regtype('text'), to_regtype('nope')") == [
        (23, 25, None)
    ]
    run_sql(storage, "db", "create type mood as enum ('sad', 'ok')", session=session)
    # User-declared types resolve — FROM-less, and inside a catalog WHERE.
    (oid,) = rows("select to_regtype('mood')")[0]
    assert isinstance(oid, int) and oid >= 65000
    assert rows("select typname from pg_type t where t.oid = to_regtype('mood')") == [("mood",)]
    # ``oid::regtype::text`` renders the user type's name.
    assert rows(f"select {oid}::regtype::text") == [("mood",)]


def test_psycopg_enum_fetch_query_shape(storage, session):
    run_sql(storage, "db", "create type mood as enum ('sad', 'ok', 'happy')", session=session)
    rows = run_sql(
        storage,
        "db",
        """SELECT name, oid, array_oid, regtype, array_agg(label) AS labels
FROM (
    SELECT
        t.typname AS name, t.oid AS oid, t.typarray AS array_oid,
        t.oid::regtype::text AS regtype, e.enumlabel AS label
    FROM pg_type t
    LEFT JOIN  pg_enum e
    ON e.enumtypid = t.oid
    WHERE t.oid = to_regtype('mood')
    ORDER BY e.enumsortorder
) x
GROUP BY name, oid, array_oid, regtype""",
        session=session,
    )[-1].rows
    assert len(rows) == 1
    name, _oid, _arr, regtype, labels = rows[0]
    assert (name, regtype) == ("mood", "mood")
    assert list(labels) == ["sad", "ok", "happy"]


def test_create_schema_and_qualified_types(storage, session):
    def rows(sql):
        return run_sql(storage, "db", sql, session=session)[-1].rows

    run_sql(storage, "db", "create schema if not exists testschema", session=session)
    run_sql(storage, "db", "create schema if not exists testschema", session=session)  # idempotent
    with pytest.raises(Exception) as exc:
        run_sql(storage, "db", "create schema testschema", session=session)
    assert getattr(exc.value, "sqlstate", None) == "42P06"

    run_sql(
        storage,
        "db",
        "create type testschema.testcomp as (foo text, bar int8)",
        session=session,
    )
    # pg_namespace carries the schema; pg_type splits the dotted name.
    assert rows("select nspname from pg_namespace where nspname = 'testschema'") == [
        ("testschema",)
    ]
    got = rows(
        "select t.typname, n.nspname from pg_type t join pg_namespace n"
        " on t.typnamespace = n.oid where t.typname = 'testcomp'"
    )
    assert got == [("testcomp", "testschema")]
    # Qualified resolution: to_regtype and the ::regtype literal cast.
    (oid,) = rows("select to_regtype('testschema.testcomp')")[0]
    assert rows("select typname from pg_type where oid = 'testschema.testcomp'::regtype") == [
        ("testcomp",)
    ]
    assert rows(f"select {oid}::regtype::text") == [("testschema.testcomp",)]

    # DROP SCHEMA: dependency error without CASCADE, cascade drops the types.
    with pytest.raises(Exception) as exc:
        run_sql(storage, "db", "drop schema testschema", session=session)
    assert getattr(exc.value, "sqlstate", None) == "2BP01"
    run_sql(storage, "db", "drop schema testschema cascade", session=session)
    assert rows("select to_regtype('testschema.testcomp')") == [(None,)]
    run_sql(storage, "db", "drop schema if exists testschema", session=session)
    with pytest.raises(Exception) as exc:
        run_sql(storage, "db", "drop schema testschema", session=session)
    assert getattr(exc.value, "sqlstate", None) == "3F000"
    # DROP TYPE IF EXISTS tolerates the missing schema.
    run_sql(storage, "db", "drop type if exists testschema.testcomp cascade", session=session)


# -- catalog builders are consistent under concurrent DDL --------------------- #


class _RacingCatalog:
    """A real ``Catalog`` that hides one existing table on its first listing.

    That is what a builder sees when another session commits a ``CREATE TABLE``
    mid-scan: the first enumeration (which assigns the OIDs) misses the table
    and the second one returns it. The wrapper defers every lookup to the
    genuine catalog, so each table still resolves against real storage.
    """

    def __init__(self, inner, hidden: str) -> None:
        self._inner = inner
        self._hidden = hidden
        self.calls = 0

    def list_tables(self, db: str):
        self.calls += 1
        names = list(self._inner.list_tables(db))
        if self.calls == 1:
            names = [n for n in names if n != self._hidden]
        return names

    def __getattr__(self, name):
        return getattr(self._inner, name)


@pytest.mark.parametrize(
    "builder",
    ["_pg_class", "_pg_attribute", "_pg_attrdef", "_pg_description", "_pg_index"],
)
def test_catalog_builders_survive_a_table_appearing_mid_scan(storage, session, builder):
    """A builder that enumerates the tables twice — once for the OID map, once
    for the rows — dies with a ``KeyError`` on a table the first pass never saw.
    Each must take a single snapshot instead.
    """
    from secantus.sql import virtual
    from secantus.sql.catalog import Catalog

    q(storage, session, "CREATE TABLE seen (id int PRIMARY KEY, v text)")
    q(storage, session, "CREATE TABLE create_and_drop_table (id int PRIMARY KEY, v text)")
    racing = _RacingCatalog(Catalog(storage), "create_and_drop_table")
    assert isinstance(getattr(virtual, builder)(DB, session, storage, racing), list)


def test_max_index_keys_setting(storage, session):
    # pgjdbc's getMaxIndexKeys reads this once per connection; every FK /
    # primary-key metadata call errors if the row is absent.
    res = q(
        storage, session, "SELECT setting FROM pg_catalog.pg_settings WHERE name='max_index_keys'"
    )
    assert res.rows == [("32",)]


def test_pg_proc_arg_mode_columns(storage, session):
    # pgjdbc's getFunctionColumns selects proargmodes / proallargtypes; NULL is
    # a valid value (no OUT params) but the columns must exist.
    q(storage, session, "CREATE FUNCTION f1(int) RETURNS int AS 'SELECT 1' LANGUAGE sql")
    res = q(
        storage,
        session,
        "SELECT proargmodes, proallargtypes FROM pg_proc WHERE proname='f1'",
    )
    assert res.rows == [(None, None)]


def test_pg_class_reltuples(storage, session):
    # pgjdbc's getIndexInfo reads ci.reltuples as CARDINALITY; -1 is PG's
    # "no estimate yet" initial value.
    q(storage, session, "CREATE TABLE rt (a int PRIMARY KEY)")
    res = q(storage, session, "SELECT reltuples FROM pg_class WHERE relname='rt'")
    assert res.rows == [(-1.0,)]


def test_join_order_by_computed_output_alias(storage, session):
    # pgjdbc's getTables ORDER BY "TABLE_TYPE" names a computed (CASE) output
    # alias; the evaluated-join planner must substitute the select expression
    # (input-column resolution alone raises 42703).
    q(storage, session, "CREATE TABLE ta (x int)")
    q(storage, session, "CREATE TABLE tb (y int)")
    q(storage, session, "INSERT INTO ta VALUES (1)")
    q(storage, session, "INSERT INTO ta VALUES (2)")
    q(storage, session, "INSERT INTO tb VALUES (1)")
    q(storage, session, "INSERT INTO tb VALUES (2)")
    res = q(
        storage,
        session,
        "SELECT CASE a.x WHEN 1 THEN 'one' ELSE 'two' END AS \"AA\""
        ' FROM ta a, tb b WHERE a.x = b.y ORDER BY "AA" DESC',
    )
    assert res.rows == [("two",), ("one",)]
    # ordinals resolve the same way
    res = q(
        storage,
        session,
        "SELECT CASE a.x WHEN 1 THEN 'one' ELSE 'two' END"
        " FROM ta a, tb b WHERE a.x = b.y ORDER BY 1",
    )
    assert res.rows == [("one",), ("two",)]


def test_pgjdbc_get_tables_query_shape(storage, session):
    # The structural skeleton of pgjdbc's getTables: comma-join + LEFT JOIN
    # pg_description + CASE-computed "TABLE_TYPE" + quoted-alias ORDER BY.
    q(storage, session, "CREATE TABLE mdt (id int4)")
    q(storage, session, "COMMENT ON TABLE mdt IS 'a comment'")
    res = q(
        storage,
        session,
        'SELECT n.nspname AS "TABLE_SCHEM", c.relname AS "TABLE_NAME",'
        " CASE c.relkind WHEN 'r' THEN 'TABLE' ELSE NULL END AS \"TABLE_TYPE\","
        ' d.description AS "REMARKS"'
        " FROM pg_catalog.pg_namespace n, pg_catalog.pg_class c"
        " LEFT JOIN pg_catalog.pg_description d ON (c.oid = d.objoid"
        " AND d.objsubid = 0 and d.classoid = 'pg_class'::regclass)"
        " WHERE c.relnamespace = n.oid AND n.nspname LIKE 'public'"
        " AND c.relkind = 'r'"
        ' ORDER BY "TABLE_TYPE","TABLE_SCHEM","TABLE_NAME"',
    )
    assert res.rows == [("public", "mdt", "TABLE", "a comment")]


def test_pg_constraint_fk_conindid_and_action_codes(storage, session):
    # A foreign key's conindid points at the referenced table's PK index and
    # carries the one-letter referential-action codes — pgjdbc's
    # getImportedKeys joins pkic.oid = con.conindid and decodes
    # confupdtype/confdeltype; conindid 0 silently empties the result.
    q(storage, session, "CREATE TABLE pkt (a int, b int, PRIMARY KEY (a, b))")
    q(
        storage,
        session,
        "CREATE TABLE fkt (x int, y int, FOREIGN KEY (x, y) REFERENCES pkt (a, b)"
        " ON DELETE CASCADE ON UPDATE SET NULL)",
    )
    res = q(
        storage,
        session,
        "SELECT con.conindid, con.confupdtype, con.confdeltype, pkic.relname"
        " FROM pg_constraint con, pg_class pkic"
        " WHERE con.contype = 'f' AND pkic.oid = con.conindid",
    )
    assert res.rows == [(res.rows[0][0], "n", "c", "pkt_pkey")]


def test_pgjdbc_get_imported_keys_shape(storage, session):
    # The core of pgjdbc's getImportedKeys: position-joined conkey/confkey
    # via generate_series, PK index join through conindid. Two rows for a
    # two-column FK, KEY_SEQ 1 and 2.
    q(storage, session, "CREATE TABLE pkt (a int, b int, PRIMARY KEY (a, b))")
    q(storage, session, "CREATE TABLE fkt (x int, y int, FOREIGN KEY (x, y) REFERENCES pkt (a, b))")
    res = q(
        storage,
        session,
        "SELECT pka.attname, fka.attname, pos.n, con.conname, pkic.relname"
        " FROM pg_catalog.pg_class pkc, pg_catalog.pg_attribute pka,"
        " pg_catalog.pg_class fkc, pg_catalog.pg_attribute fka,"
        " pg_catalog.pg_constraint con, pg_catalog.generate_series(1, 4) pos(n),"
        " pg_catalog.pg_class pkic"
        " WHERE pkc.oid = pka.attrelid AND pka.attnum = con.confkey[pos.n]"
        " AND con.confrelid = pkc.oid"
        " AND fkc.oid = fka.attrelid AND fka.attnum = con.conkey[pos.n]"
        " AND con.conrelid = fkc.oid AND con.contype = 'f'"
        " AND (pkic.relkind = 'i' OR pkic.relkind = 'I') AND pkic.oid = con.conindid"
        " AND fkc.relname = 'fkt'"
        " ORDER BY pos.n",
    )
    assert res.rows == [
        ("a", "x", 1, "fkt_x_fkey", "pkt_pkey"),
        ("b", "y", 2, "fkt_x_fkey", "pkt_pkey"),
    ]


def test_comment_on_domain_and_obj_description(storage, session):
    # pgjdbc's getUDTs reads a domain's REMARKS via obj_description(oid,
    # 'pg_type'); COMMENT ON DOMAIN arrives as a sqlglot Command fallback,
    # including the IS NULL removal (rewritten to the uncomment sentinel).
    q(storage, session, "CREATE DOMAIN testint8 AS int8")
    assert q(storage, session, "comment on domain testint8 is 'jdbc123'").command_tag == "COMMENT"
    res = q(
        storage,
        session,
        "SELECT obj_description(t.oid, 'pg_type') FROM pg_type t WHERE t.typname = 'testint8'",
    )
    assert res.rows == [("jdbc123",)]
    q(storage, session, "comment on domain testint8 is NULL")
    res = q(
        storage,
        session,
        "SELECT obj_description(t.oid, 'pg_type') FROM pg_type t WHERE t.typname = 'testint8'",
    )
    assert res.rows == [(None,)]


def test_comment_on_index_reflects_in_pg_description(storage, session):
    # remarkIndexInfo: getIndexInfo LEFT JOINs pg_description on the index
    # relation's oid to read REMARKS.
    q(storage, session, "CREATE TABLE ct (a int primary key)")
    q(storage, session, "CREATE INDEX idx_name ON ct (a)")
    assert (
        q(storage, session, "comment on index idx_name is 'index_comment'").command_tag == "COMMENT"
    )
    res = q(
        storage,
        session,
        "SELECT d.description FROM pg_class ci"
        " LEFT JOIN pg_description d ON (ci.oid = d.objoid)"
        " WHERE ci.relname = 'idx_name'",
    )
    assert res.rows == [("index_comment",)]

    with pytest.raises(errors.SQLError) as e:
        q(storage, session, "comment on index no_such_index is 'x'")
    assert e.value.sqlstate == "42704"


def test_pg_get_keywords_and_sql_keywords_query(storage, session):
    # pgjdbc's getSQLKeywords: string_agg over the keywords SRF with a
    # <> ALL array filter. reindex must be present (the test asserts it).
    res = q(
        storage,
        session,
        "SELECT string_agg(word, ',') FROM pg_catalog.pg_get_keywords()"
        " WHERE word <> ALL ('{abort,do}'::text[])",
    )
    words = res.rows[0][0].split(",")
    assert "reindex" in words
    assert "abort" not in words and "do" not in words
    assert len(words) == len(set(words))


def test_aggregates_over_srf_from(storage, session):
    assert q(storage, session, "SELECT sum(g) FROM generate_series(1, 3) g").rows == [(6,)]
    assert q(storage, session, "SELECT string_agg('ab', '') FROM generate_series(1, 3)").rows == [
        ("ababab",)
    ]
    assert q(storage, session, "SELECT array_agg(g) FROM generate_series(1,3) g").rows == [
        ([1, 2, 3],)
    ]


def test_scalar_subquery_over_srf(storage, session):
    res = q(storage, session, "SELECT (SELECT string_agg('ab', '') FROM generate_series(1, 3))")
    assert res.rows == [("ababab",)]


def test_function_wrapped_string_agg(storage, session):
    q(storage, session, "CREATE TABLE wsa (b text)")
    q(storage, session, "INSERT INTO wsa VALUES ('61'), ('62')")
    assert q(storage, session, "SELECT decode(string_agg(b, ''), 'hex') FROM wsa").rows == [
        (b"ab",)
    ]
    assert q(storage, session, "SELECT upper(string_agg(b, '-')) FROM wsa").rows == [("61-62",)]


def test_pg_database_includes_postgres_maintenance_db(storage, session):
    # pgjdbc's getCatalogs asserts both the connected db and "postgres" are
    # present and the list is sorted; a PG client must never see MongoDB-side
    # names like "local" as a connectable catalog.
    rows = q(
        storage,
        session,
        'SELECT datname AS "TABLE_CAT" FROM pg_catalog.pg_database'
        " WHERE datallowconn = true ORDER BY datname",
    ).rows
    names = [r[0] for r in rows]
    assert "postgres" in names
    assert DB in names
    assert "local" not in names
    assert names == sorted(names)


def test_comma_join_is_keyed_not_cartesian(storage, session):
    # A multi-table comma-join with join predicates in WHERE must key each
    # $lookup instead of cross-producting — pgjdbc's getImportedKeys over the
    # catalogs otherwise materializes billions of rows (183GB OOM). It now
    # completes and returns the FK's key columns with their positions.
    q(storage, session, "CREATE TABLE pk (a int, b int, PRIMARY KEY (a, b))")
    q(
        storage,
        session,
        "CREATE TABLE fk (x int, y int, FOREIGN KEY (x, y) REFERENCES pk(a, b))",
    )
    res = q(
        storage,
        session,
        "SELECT pka.attname, fka.attname, pos.n"
        " FROM pg_catalog.pg_namespace pkn, pg_catalog.pg_class pkc,"
        " pg_catalog.pg_attribute pka, pg_catalog.pg_namespace fkn,"
        " pg_catalog.pg_class fkc, pg_catalog.pg_attribute fka,"
        " pg_catalog.pg_constraint con, pg_catalog.generate_series(1, 32) pos(n),"
        " pg_catalog.pg_class pkic"
        " WHERE pkn.oid = pkc.relnamespace AND pkc.oid = pka.attrelid"
        " AND pka.attnum = con.confkey[pos.n] AND con.confrelid = pkc.oid"
        " AND fkn.oid = fkc.relnamespace AND fkc.oid = fka.attrelid"
        " AND fka.attnum = con.conkey[pos.n] AND con.conrelid = fkc.oid"
        " AND con.contype = 'f' AND pkic.oid = con.conindid"
        " ORDER BY pos.n",
    )
    assert res.rows == [("a", "x", 1), ("b", "y", 2)]


def test_comma_join_semantics_preserved(storage, session):
    q(storage, session, "CREATE TABLE ca (id int, x int)")
    q(storage, session, "CREATE TABLE cb (id int, aid int)")
    q(storage, session, "CREATE TABLE cc (id int, bid int)")
    q(storage, session, "INSERT INTO ca VALUES (1, 10), (2, 20)")
    q(storage, session, "INSERT INTO cb VALUES (100, 1), (200, 2)")
    q(storage, session, "INSERT INTO cc VALUES (1000, 100), (2000, 200)")
    res = q(
        storage,
        session,
        "SELECT ca.x, cc.id FROM ca, cb, cc"
        " WHERE ca.id = cb.aid AND cb.id = cc.bid AND ca.x = 10 ORDER BY cc.id",
    )
    assert res.rows == [(10, 1000)]
