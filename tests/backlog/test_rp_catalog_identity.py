"""Built-in catalog rows against PostgreSQL 15.19 (found while speeding up
the `information_schema` views, RP-131).

Comparing `information_schema.routines`, `parameters` and `element_types`
with PostgreSQL 15.19 row for row found answers that differed for reasons of
the catalogs underneath them: `pg_type` rows of built-in types (RP-300),
`format_type` of the one-byte `"char"` and of a modifier on a type with no
`typmodout` (RP-301), and `pg_attribute` rows of the built-in relations
(RP-302). Every literal here was measured on PostgreSQL 15.19 with
read-only queries.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import pg_answer


@pytest.mark.backlog_fixed("RP-300")
def test_builtin_types_carry_postgres_identity_columns(pg: Any) -> None:
    """A fixed-length subscriptable type's element (`name`'s is `"char"`,
    `point`'s `float8`), a range array's `typtype`, `_box`'s delimiter and
    the collatable types' collation, as `initdb` records them. This server
    derived them and got 56 rows wrong."""
    got = pg_answer(
        pg,
        "select typname::text, typtype::text, typdelim::text, typelem::int, typnotnull,"
        " typbasetype::int, typtypmod, typcollation::int from pg_type where typname in"
        " ('name', 'int2vector', 'point', '_int4range', '_text', 'record', 'pg_node_tree',"
        " '_box') order by oid",
    )
    assert got == (
        "ok",
        [
            ("name", "b", ",", 18, False, 0, -1, 950),
            ("int2vector", "b", ",", 21, False, 0, -1, 0),
            ("pg_node_tree", "b", ",", 0, False, 0, -1, 100),
            ("point", "b", ",", 701, False, 0, -1, 0),
            ("_text", "b", ",", 25, False, 0, -1, 100),
            ("_box", "b", ";", 603, False, 0, -1, 0),
            ("record", "p", ",", 0, False, 0, -1, 0),
            ("_int4range", "b", ",", 3904, False, 0, -1, 0),
        ],
    )


@pytest.mark.backlog_fixed("RP-300")
def test_information_schema_reports_a_vector_type_as_an_array(pg: Any) -> None:
    """`information_schema` calls a type ARRAY when its `typelem` is set and
    its `typlen` is -1, which `int2vector` and `oidvector` are."""
    got = pg_answer(
        pg,
        "select data_type::text from information_schema.parameters"
        " where specific_name = 'int2vectorout_41'",
    )
    assert got == ("ok", [("ARRAY",)])


@pytest.mark.backlog_fixed("RP-301")
def test_format_type_of_the_one_byte_char(pg: Any) -> None:
    """Oid 18 is `"char"`, not `character`; so is what `information_schema`
    reports for a function over it."""
    got = pg_answer(
        pg,
        "select format_type(18, null), format_type(1002, null),"
        " (select data_type::text from information_schema.parameters"
        " where specific_name = 'charout_33')",
    )
    assert got == ("ok", [('"char"', '"char"[]', '"char"')])


@pytest.mark.backlog_fixed("RP-301")
def test_format_type_prints_a_modifier_no_typmodout_reads(pg: Any) -> None:
    """`format_type_extended` prints its special-cased types' modifiers by
    their own rules (or not at all) and any other type's in parentheses."""
    got = pg_answer(
        pg,
        "select format_type(23, 5), format_type(25, 7), format_type(16, 0),"
        " format_type(1043, 6), format_type(1700, 196612), format_type(1083, 9),"
        " format_type(3614, 2), format_type(1042, 3), format_type(18, 3),"
        " format_type(1009, 7)",
    )
    assert got == (
        "ok",
        [
            (
                "integer",
                "text(7)",
                "boolean",
                "character varying(2)",
                "numeric(3,0)",
                "time(9) without time zone",
                "tsvector(2)",
                "character",
                '"char"(3)',
                "text(7)[]",
            )
        ],
    )


@pytest.mark.backlog("RP-302")
def test_builtin_relations_attributes(pg: Any) -> None:
    """`pg_class` lists its six system columns (attnum < 0) as every
    relation with storage does; a catalog's collatable column is collated
    `C` (950); an array column records one dimension."""
    got = pg_answer(
        pg,
        "select (select count(*) from pg_attribute where attrelid = 'pg_class'::regclass),"
        " (select count(*) from pg_attribute where attrelid = 'pg_class'::regclass"
        " and attnum < 0),"
        " (select attcollation::int from pg_attribute where attrelid = 'pg_aggregate'::regclass"
        " and attname = 'agginitval'),"
        " (select attndims from pg_attribute where attrelid = 'pg_proc'::regclass"
        " and attname = 'proconfig')",
    )
    assert got == ("ok", [(39, 6, 950, 1)])


@pytest.mark.backlog_fixed("RP-285")
@pytest.mark.parametrize("in_block", [False, True])
def test_an_index_names_its_access_method_in_relam(pg: Any, in_block: bool) -> None:
    """`pg_class.relam` is the index's own method (found while modelling
    GIN and GiST under SERIALIZABLE, RP-254); every index here said `btree`,
    so a join to `pg_am` named the wrong method."""
    if in_block:
        pg.execute("begin")
    try:
        pg.execute("create table rp285_t (a int[], p point, b int)")
        pg.execute("create index rp285_g on rp285_t using gin (a)")
        pg.execute("create index rp285_s on rp285_t using gist (p)")
        pg.execute("create index rp285_h on rp285_t using hash (b)")
        pg.execute("create index rp285_b on rp285_t (b)")
        got = pg_answer(
            pg,
            "select c.relname::text, am.amname::text from pg_class c"
            " join pg_am am on am.oid = c.relam where c.relname like 'rp285_%'"
            " and c.relkind = 'i' order by 1",
        )
    finally:
        if in_block:
            pg.execute("rollback")
        else:
            pg.execute("drop table rp285_t")
    assert got == (
        "ok",
        [("rp285_b", "btree"), ("rp285_g", "gin"), ("rp285_h", "hash"), ("rp285_s", "gist")],
    )
