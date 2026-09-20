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


def test_pg_proc_argtypes_for_unnamed_parameters(storage, session):
    """`f(int, int)` must record int4, not void.

    sqlglot parses an UNNAMED parameter as a bare `Identifier` rather than a
    `ColumnDef` carrying a `DataType`, so the type tag came back `None` and
    `_type_oid` mapped it to **2278 (void)** — an OID this catalog does not
    even define, so a client resolving it found nothing. pgjdbc's
    `DatabaseMetaDataTest::functionColumns` creates exactly `f1(int, int)` and
    reads the argument rows back.

    The NAMED form is asserted beside it because it was always correct: the
    bug lived only in the branch that handles parameters without names, which
    is why it survived a catalog full of working functions.
    """
    q(storage, session, "CREATE FUNCTION fu(int, int) RETURNS int AS 'SELECT 1' LANGUAGE sql")
    q(storage, session, "CREATE FUNCTION fn(a int, b text) RETURNS int AS 'SELECT 1' LANGUAGE sql")
    q(storage, session, "CREATE FUNCTION fz() RETURNS int AS 'SELECT 1' LANGUAGE sql")

    def argtypes(name: str) -> str:
        res = q(storage, session, f"SELECT proargtypes FROM pg_proc WHERE proname='{name}'")
        return res.rows[0][0]

    assert argtypes("fu") == "23 23", "unnamed int params must be int4 (23), not void (2278)"
    assert argtypes("fn") == "23 25"
    assert argtypes("fz") == ""


def test_pg_proc_argtypes_resolve_multiword_type_names(storage, session):
    """`double precision` is two words and still one unnamed parameter.

    The Identifier branch resolves the whole spelling, so a multi-word builtin
    must not fall back to void the way a single-word one used to.
    """
    q(
        storage,
        session,
        "CREATE FUNCTION fd(double precision) RETURNS int AS 'SELECT 1' LANGUAGE sql",
    )
    res = q(storage, session, "SELECT proargtypes FROM pg_proc WHERE proname='fd'")
    assert res.rows[0][0] == "701", "double precision is float8 (701)"


def test_declared_char_types_have_pg_type_rows(storage, session):
    """A ``varchar`` / ``char(n)`` column must point at a type that EXISTS.

    Both spellings fold to the ``text`` storage tag, but a column still records
    the declared oid (1043 / 1042). ``pg_type`` is built from the tag-keyed
    ``PG_TYPENAME``, which can name only one of the three — so every such
    column pointed at an oid with NO ``pg_type`` row, and a client joining
    ``pg_attribute`` to ``pg_type`` (which is what a JDBC/psycopg metadata call
    does) resolved it to nothing.

    Asserted as the join a driver actually performs, not as a row count, so it
    fails if either half regresses. ``text`` is asserted beside them to pin
    that the fold still works.
    """
    q(storage, session, "CREATE TABLE ct (a varchar(10), b char(5), c text)")
    res = q(
        storage,
        session,
        "SELECT a.attname, a.atttypid, t.typname "
        "FROM pg_attribute a JOIN pg_class c ON a.attrelid = c.oid "
        "LEFT JOIN pg_type t ON t.oid = a.atttypid "
        "WHERE c.relname = 'ct' ORDER BY a.attnum",
    )
    assert res.rows == [
        ("a", 1043, "varchar"),
        ("b", 1042, "bpchar"),
        ("c", 25, "text"),
    ]


def test_columns_data_type_renders_the_declared_char_type(storage, session):
    """``information_schema.columns.data_type`` names the DECLARED type.

    A column already carried the declared oid for ``pg_attribute.atttypid``;
    only this render ignored it and reported the ``text`` storage tag for every
    string column. PostgreSQL 14 reports ``character varying`` / ``character``
    (measured 2026-09-19), and a bare ``varchar`` renders the same as a
    length-qualified one.
    """
    q(storage, session, "CREATE TABLE cd (a varchar(10), b char(5), c text, d varchar)")
    res = q(
        storage,
        session,
        "SELECT data_type FROM information_schema.columns "
        "WHERE table_name = 'cd' ORDER BY ordinal_position",
    )
    assert [r[0] for r in res.rows] == [
        "character varying",
        "character",
        "text",
        "character varying",
    ]


def test_pg_proc_argtypes_record_the_declared_char_type(storage, session):
    """``f(int, varchar)`` records 1043, not the ``text`` tag's 25.

    Parameters had no equivalent of a column's ``decl_oid``, so the declared
    spelling was lost and ``proargtypes`` read ``'23 25'`` where PostgreSQL 14
    records ``'23 1043'`` (measured 2026-09-19). Both the unnamed and the named
    form are asserted: the two take different parse branches, and the previous
    bug in this area lived in only one of them.
    """
    q(storage, session, "CREATE FUNCTION fv(int, varchar) RETURNS int AS 'SELECT 1' LANGUAGE sql")
    q(
        storage,
        session,
        "CREATE FUNCTION fw(a int, b varchar, c char(3), d text) "
        "RETURNS int AS 'SELECT 1' LANGUAGE sql",
    )
    q(storage, session, "CREATE FUNCTION fb(bpchar) RETURNS int AS 'SELECT 1' LANGUAGE sql")

    def argtypes(name: str) -> str:
        res = q(storage, session, f"SELECT proargtypes FROM pg_proc WHERE proname='{name}'")
        return res.rows[0][0]

    assert argtypes("fv") == "23 1043", "bare varchar is 1043, not text's 25"
    assert argtypes("fw") == "23 1043 1042 25"
    # Bare `bpchar` is the worse half of the same family: it has no storage tag
    # at all, so it recorded 2278 (void) rather than merely the wrong string
    # type.
    assert argtypes("fb") == "1042", "bare bpchar is 1042, not void (2278)"


def test_parameters_data_type_renders_the_declared_char_type(storage, session):
    """``information_schema.parameters.data_type`` renders the SQL name.

    PostgreSQL 14 renders ``character varying`` / ``character`` there — the SQL
    spelling, NOT the ``pg_type.typname`` (``varchar`` / ``bpchar``) the same
    type reports elsewhere. Measured 2026-09-19; the two surfaces disagreeing
    is why this is asserted separately from ``proargtypes``.
    """
    q(
        storage,
        session,
        "CREATE FUNCTION fp(a int, b varchar, c char(3), d text) "
        "RETURNS int AS 'SELECT 1' LANGUAGE sql",
    )
    res = q(
        storage,
        session,
        "SELECT data_type FROM information_schema.parameters "
        "WHERE specific_name LIKE 'fp%' ORDER BY ordinal_position",
    )
    assert [r[0] for r in res.rows] == [
        "integer",
        "character varying",
        "character",
        "text",
    ]


def test_pg_class_relpages(storage, session):
    """`pg_class.relpages` exists, and an INDEX reports 1 page rather than 0.

    pgjdbc's `getIndexInfo` selects `ci.relpages AS PAGES`, so the column being
    absent failed the whole query with `column "relpages" does not exist` —
    taking out `ascDescIndexInfo`, `partialIndexInfo` and `remarkIndexInfo`
    before any assertion ran.

    A fresh table reports 0 and a fresh index reports 1 on PostgreSQL 14
    (measured 2026-09-19): an index has its metapage from the moment it
    exists. The index case is asserted because 0 would be the obvious guess
    and it is wrong.
    """
    q(storage, session, "CREATE TABLE rp (a int PRIMARY KEY, b text)")
    q(storage, session, "CREATE INDEX rp_b ON rp(b)")
    res = q(
        storage,
        session,
        "SELECT relname, relpages FROM pg_class WHERE relname IN ('rp', 'rp_b') ORDER BY relname",
    )
    assert res.rows == [("rp", 0), ("rp_b", 1)]


def test_pg_type_typlen(storage, session):
    """`pg_type.typlen`, including the `name` row pgjdbc needs to connect well.

    `getMaxNameLength()` selects `typlen` for `typname = 'name'` in
    `pg_catalog` and raises "Unable to find name datatype in the system
    catalogs" when the row is missing — which is what broke
    `getClientInfoProperties`. `name` is not a type this server stores; the row
    exists only so that lookup resolves.

    The array row is asserted too: TypeInfoCache's array lookup filters on
    `typlen = -1`, so a wrong value there silently hides every array type.
    All values measured against PostgreSQL 14 on 2026-09-19.
    """
    res = q(
        storage,
        session,
        "SELECT t.typlen FROM pg_catalog.pg_type t, pg_catalog.pg_namespace n "
        "WHERE t.typnamespace = n.oid AND t.typname = 'name' "
        "AND n.nspname = 'pg_catalog'",
    )
    assert res.rows == [(64,)], "name is 64 bytes; pgjdbc reads it as NAMEDATALEN"

    res = q(
        storage,
        session,
        "SELECT typname, typlen FROM pg_type "
        "WHERE typname IN ('int4', 'bool', 'uuid', 'text', 'varchar', '_text') "
        "ORDER BY typname",
    )
    assert res.rows == [
        ("_text", -1),
        ("bool", 1),
        ("int4", 4),
        ("text", -1),
        ("uuid", 16),
        ("varchar", -1),
    ]


def test_pg_type_typlen_for_user_types(storage, session):
    """An enum is 4 bytes; every other user type is varlena.

    Measured on PostgreSQL 14 (2026-09-19). The enum is the one case where the
    -1 default would be wrong — it is a fixed 4-byte oid reference, not a
    varlena — so it is set explicitly and pinned here.
    """
    q(storage, session, "CREATE TYPE te AS ENUM ('a', 'b')")
    q(storage, session, "CREATE TYPE tc AS (x int, y text)")
    res = q(
        storage,
        session,
        "SELECT typname, typlen FROM pg_type WHERE typname IN ('te', 'tc') ORDER BY typname",
    )
    assert res.rows == [("tc", -1), ("te", 4)]


def test_schema_qualified_function_reports_its_namespace(storage, session):
    """`CREATE FUNCTION hf.addf(...)` belongs to `hf`, not `public`.

    Only a `pg_temp_` qualifier was preserved at creation, so any other schema
    was silently dropped and the function reported `pronamespace = public`. It
    existed and was invisible in the schema it was created in — pgjdbc's
    `getFunctions` and `getProcedures` both filter by schema, which is what
    took out `getFunctionsInSchemaForFunctions`.

    `proname` is asserted bare: the dotted key is storage, not the name a
    client reads.
    """
    q(storage, session, "CREATE SCHEMA hf")
    q(storage, session, "CREATE FUNCTION hf.addf(int, int) RETURNS int AS 'SELECT 1' LANGUAGE sql")
    q(storage, session, "CREATE FUNCTION plainf(int) RETURNS int AS 'SELECT 1' LANGUAGE sql")
    res = q(
        storage,
        session,
        "SELECT p.proname, n.nspname FROM pg_proc p "
        "JOIN pg_namespace n ON p.pronamespace = n.oid "
        "WHERE p.proname IN ('addf', 'plainf') ORDER BY p.proname",
    )
    assert res.rows == [("addf", "hf"), ("plainf", "public")]


def test_schema_qualified_function_call_resolution(storage, session):
    """A schema-homed function is callable QUALIFIED and not bare.

    Both halves matter. Storing the dotted key without teaching the call path
    about it made `hf.addf(2, 3)` raise "function addf does not exist" — the
    namespace would have been right and the function unusable.

    The bare call failing is not a regression but a fidelity fix: PostgreSQL 14
    raises `function addf(integer, integer) does not exist` for it too
    (measured 2026-09-19), because `hf` is not on the search_path. This server
    used to answer it.
    """
    q(storage, session, "CREATE SCHEMA hf")
    q(
        storage,
        session,
        "CREATE FUNCTION hf.addf(int, int) RETURNS int AS 'SELECT $1 + $2' LANGUAGE sql",
    )
    assert q(storage, session, "SELECT hf.addf(2, 3)").rows == [(5,)]

    with pytest.raises(errors.SQLError) as exc:
        q(storage, session, "SELECT addf(2, 3)")
    assert "does not exist" in str(exc.value)


def test_enum_array_column_reports_the_array_type(storage, session):
    """A ``ts.te[]`` column resolves to the ARRAY type, not the enum itself.

    The enum branch of ``pg_attribute`` had no array handling — the composite
    branch beside it did — so an array-of-enum column recorded the element's
    oid. pgjdbc's ``getColumns`` then reported TYPE_NAME
    ``"test_schema"."test_enum"`` and the element's DATA_TYPE where
    PostgreSQL 14 reports ``"test_schema"."_test_enum"`` and ARRAY.

    The scalar column is asserted beside it: the fix must not turn every enum
    column into an array.
    """
    q(storage, session, "CREATE SCHEMA ts")
    q(storage, session, "CREATE TYPE ts.te AS ENUM ('v')")
    q(storage, session, "CREATE TABLE ea (arr ts.te[], sc ts.te)")
    res = q(
        storage,
        session,
        "SELECT a.attname, t.typname, t.typtype FROM pg_attribute a "
        "JOIN pg_class c ON a.attrelid = c.oid "
        "JOIN pg_type t ON t.oid = a.atttypid "
        "WHERE c.relname = 'ea' AND a.attnum > 0 ORDER BY a.attnum",
    )
    assert res.rows == [("arr", "_te", "b"), ("sc", "te", "e")]


def test_array_type_names_collide_per_namespace(storage, session):
    """An array type name is unique per SCHEMA, not globally.

    PostgreSQL prepends underscores until the name is free **within its own
    namespace**. A single global set made ``test_schema.test_enum``'s array
    dodge the unrelated ``public._test_enum`` and come out ``__test_enum``.

    Both halves are pinned because they pull in opposite directions: the
    schema-scoped array keeps ONE underscore even though ``public._test_enum``
    exists, while the public array really does need THREE — ``_test_enum`` is
    taken by the enum itself and ``__test_enum`` by that enum's own array,
    which is created first because it has the lower oid.

    Every name here was measured against PostgreSQL 14 on 2026-09-20.
    """
    q(storage, session, "CREATE SCHEMA test_schema")
    q(storage, session, "CREATE TYPE test_schema.test_enum AS ENUM ('val')")
    q(storage, session, "CREATE TYPE _test_enum AS ENUM ('evil')")
    q(storage, session, "CREATE TYPE test_enum AS ENUM ('other')")
    q(
        storage,
        session,
        "CREATE TABLE on_path_table (a test_schema.test_enum[], b _test_enum, c test_enum[])",
    )
    res = q(
        storage,
        session,
        "SELECT a.attname, n.nspname, t.typname FROM pg_attribute a "
        "JOIN pg_class c ON a.attrelid = c.oid "
        "JOIN pg_type t ON t.oid = a.atttypid "
        "JOIN pg_namespace n ON t.typnamespace = n.oid "
        "WHERE c.relname = 'on_path_table' AND a.attnum > 0 ORDER BY a.attnum",
    )
    assert res.rows == [
        ("a", "test_schema", "_test_enum"),
        ("b", "public", "_test_enum"),
        ("c", "public", "___test_enum"),
    ]


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


def test_pg_proc_argmodes_for_in_inout_out(storage, session):
    """`f(IN a int, INOUT b varchar, OUT c timestamptz)` in full.

    Four separate facts, all measured against PostgreSQL 14 on 2026-09-20 and
    all previously wrong:

    - `proargmodes` was NULL; it is `{i,b,o}`.
    - `proallargtypes` was NULL; it is every parameter's type.
    - `proargtypes` listed all three, but it is the CALL signature — an
      OUT-only parameter is excluded, so it is `23 1043`.
    - `prorettype` was 2278 (void, an oid this catalog does not define); with
      two output columns it is 2249 (`record`).
    """
    q(
        storage,
        session,
        "CREATE FUNCTION f3(IN a int, INOUT b varchar, OUT c timestamptz) "
        "AS $f$ BEGIN b := 'a'; END; $f$ LANGUAGE plpgsql",
    )
    res = q(
        storage,
        session,
        "SELECT proargmodes, proallargtypes, proargnames, proargtypes, prorettype "
        "FROM pg_proc WHERE proname = 'f3'",
    )
    modes, allargs, names, argtypes, rettype = res.rows[0]
    assert list(modes) == ["i", "b", "o"]
    assert list(allargs) == [23, 1043, 1184]
    assert list(names) == ["a", "b", "c"]
    assert argtypes == "23 1043", "proargtypes is the call signature: no OUT-only param"
    assert rettype == 2249, "two output columns means record, not void"


def test_pg_proc_argmodes_are_null_when_every_param_is_in(storage, session):
    """PostgreSQL leaves both arrays NULL unless some parameter is not plain IN.

    pgjdbc's getProcedureColumns switches on exactly that, so populating them
    unconditionally would change how it reads every ordinary function.
    """
    q(
        storage,
        session,
        "CREATE FUNCTION allin(a int, b text) RETURNS int AS 'SELECT 1' LANGUAGE sql",
    )
    res = q(
        storage,
        session,
        "SELECT proargmodes, proallargtypes FROM pg_proc WHERE proname = 'allin'",
    )
    assert res.rows == [(None, None)]


def test_pg_proc_returns_table(storage, session):
    """`RETURNS TABLE (i int)` reports its columns as `t`-mode entries.

    The output columns ride the same three arrays as OUT parameters, and a
    single column makes `prorettype` that column's type — not void, and not
    `record`. `proargtypes` stays empty because the function takes no input.
    """
    q(storage, session, "CREATE FUNCTION f5() RETURNS TABLE (i int) LANGUAGE sql AS 'SELECT 1'")
    res = q(
        storage,
        session,
        "SELECT proargmodes, proallargtypes, proargnames, proargtypes, prorettype, proretset "
        "FROM pg_proc WHERE proname = 'f5'",
    )
    modes, allargs, names, argtypes, rettype, retset = res.rows[0]
    assert list(modes) == ["t"]
    assert list(allargs) == [23]
    assert list(names) == ["i"]
    assert argtypes == ""
    assert rettype == 23
    assert retset is True


def test_pg_proc_composite_return_type(storage, session):
    """`RETURNS <table>` resolves to that table's row type, not void.

    A user type has no storage tag, so the return type was 2278. The NAME is
    recorded at CREATE and resolved at reflection time, where the catalog is
    in scope — asserted against the table's own row-type oid rather than a
    literal, because these oids are this server's to mint.
    """
    q(storage, session, "CREATE TABLE mdt (id int, name text)")
    q(
        storage,
        session,
        "CREATE FUNCTION f4(int) RETURNS mdt AS $b$ SELECT 1, 'a' $b$ LANGUAGE sql",
    )
    rettype = q(storage, session, "SELECT prorettype FROM pg_proc WHERE proname = 'f4'").rows[0][0]
    rowtype = q(
        storage,
        session,
        "SELECT t.oid FROM pg_type t JOIN pg_class c ON t.typrelid = c.oid WHERE c.relname = 'mdt'",
    ).rows[0][0]
    assert rettype == rowtype != 2278


def test_pg_type_typtype_for_pseudo_and_multirange(storage, session):
    """`record` is a pseudo-type `p`; a multirange is `m`.

    Both reported `b`. This is not cosmetic: pgjdbc's getProcedureColumns
    decides whether to emit a leading `returnValue` row by switching on
    `typtype`, so `record` reading `b` gave a function with OUT parameters a
    spurious extra row — which is how this was found, after the argmodes were
    already correct. Measured on PostgreSQL 14, 2026-09-20.
    """
    res = q(
        storage,
        session,
        "SELECT typname, typtype FROM pg_type "
        "WHERE typname IN ('record', 'int4range', 'int4multirange', 'text') ORDER BY typname",
    )
    assert res.rows == [
        ("int4multirange", "m"),
        ("int4range", "r"),
        ("record", "p"),
        ("text", "b"),
    ]


def test_builtin_return_type_is_not_treated_as_a_user_type(storage, session):
    """`RETURNS refcursor` keeps its own type; it is not a composite.

    sqlglot parses `refcursor` as a USERDEFINED type name — the same shape as
    `RETURNS <composite>` — even though `type_tag_for_sql` resolves it
    perfectly well. A first version of the composite-return fix claimed every
    USERDEFINED name, which dropped refcursor's tag and made `SELECT getref()`
    describe its column as text (25) rather than refcursor (1790).

    So the discriminator is whether the type resolves to a storage tag, not
    whether sqlglot called it USERDEFINED. Both sides are asserted here
    because the bug was invisible from either one alone.
    """
    q(
        storage,
        session,
        "CREATE FUNCTION getref() RETURNS refcursor AS $b$ SELECT 'x'::refcursor $b$ LANGUAGE sql",
    )
    q(storage, session, "CREATE TABLE mdt2 (id int)")
    q(
        storage,
        session,
        "CREATE FUNCTION getcomp() RETURNS mdt2 AS $b$ SELECT 1 $b$ LANGUAGE sql",
    )
    ref = q(storage, session, "SELECT prorettype FROM pg_proc WHERE proname = 'getref'").rows[0][0]
    comp = q(storage, session, "SELECT prorettype FROM pg_proc WHERE proname = 'getcomp'").rows[0][
        0
    ]
    rowtype = q(
        storage,
        session,
        "SELECT t.oid FROM pg_type t JOIN pg_class c ON t.typrelid = c.oid "
        "WHERE c.relname = 'mdt2'",
    ).rows[0][0]
    assert ref == 1790, "refcursor resolves to its own oid, not a user type"
    assert comp == rowtype, "a real composite still resolves through the catalog"
