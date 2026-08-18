"""Which pgtest corpus files the gauge runs, and the expected divergences.

Same model as the other gauges: the corpus is NEVER modified — divergence
lives here. ``EXCLUDE`` names files not run at all (only for hangs or
crdb-internals that can't produce a meaningful non-crdb run);
``EXPECTED_DIVERGENCES`` maps a file to a one-line reason and keeps the gauge
green while the failure is a documented gap rather than a regression.
"""

#: Corpus files skipped entirely (relative to testdata/pgtest).
EXCLUDE: set[str] = set()

#: file -> reason. A file listed here that PASSES is reported loudly.
EXPECTED_DIVERGENCES: dict[str, str] = {
    "portals": (
        "portals:1182 compares the CHECK-violation MESSAGE with "
        "keepErrMessage and pins crdb's wording ('failed to satisfy CHECK "
        "constraint (a > 1.0:::FLOAT8)'). We emit real PostgreSQL's ('new row "
        "for relation \"t\" violates check constraint \"t_a_check\"'), which is "
        "what psycopg/pgjdbc users parse — matching crdb would be a fidelity "
        "REGRESSION. Everything up to :1182 passes (1182 of 1550 lines, "
        "including PortalSuspended-on-exact-MaxRows and per-Execute row "
        "counts). NOTE: the stanzas after :1182 are therefore NOT exercised — "
        "they cover 34000 'unknown portal' (already implemented, slice 21) and "
        "42P03 'cursor \"p\" already exists as portal' (NOT implemented). See "
        "tasks/backlog.md."
    ),
    "jsonpath": (
        "jsonpath:36/:76 expect crdb's BINARY jsonpath form — version byte + "
        "the SINGLE-QUOTED text ('$' -> 01272427). Real PostgreSQL's "
        "jsonpath_send emits the version byte + the canonical text WITHOUT "
        "outer quotes (0124), which is what we send. Everything else in the "
        "file is green (oid 4072, canonical $.\"abc\" text, 42601 on an "
        "empty path, jsonb_path_query)."
    ),
    "int2vector": (
        "int2vector:26 expects indoption={2} for a plain primary key — "
        "crdb's NULLS-FIRST pkey representation. Real PostgreSQL reports 0 "
        "(ASC, NULLS LAST), and so do we; matching crdb's 2 would corrupt "
        "SQLAlchemy index reflection. The BINARY int2vector encoding the "
        "stanza actually regression-tests (int2 array elements, elemoid 21 "
        "— crdb #111907 shipped int8 once) is implemented and correct."
    ),
    "row_description": (
        "row_description:376 sends `SELECT 'foo'::STRING, 'bar'::STRING(2)` "
        "with NO crdb_only marker and expects crdb's STRING aliases (text/25 "
        "and varchar/1043 typmod 6, truncating 'bar' to 'ba'). Real PostgreSQL "
        "14 rejects both casts outright — `ERROR: 42704 type \"string\" does "
        "not exist` (probed) — so the stanza can't pass against any non-crdb "
        "server, and matching crdb's varchar(2) truncation would diverge from "
        "PG. Everything before :376 is green: base-column identity across a "
        "JOIN and through a VIEW, char(n) blank padding on the wire, and "
        "attnum stability across ALTER COLUMN TYPE."
    ),
    "char": (
        "char:250 pins TableOID=105 — crdb's deterministic descriptor id, "
        "with no ignore_table_oids directive on that stanza. Real PostgreSQL "
        "reports its own pg_class oid there too (installation-specific), so "
        "the stanza can't pass against any non-crdb server. Everything else "
        "in the file is green (oid-18 \"char\": casts, columns, params, "
        "1-char truncation, NULL for empty/zero-byte, binary format)."
    ),
    "spatial": (
        "PostGIS GEOMETRY/GEOGRAPHY — an extension type outside SecantusDB's "
        "core-PostgreSQL SQL scope (the surrogate models MongoDB, not PostGIS). "
        "A GEOMETRY value can't round-trip its EWKB binary form; an untyped "
        "binary GEOMETRY parameter now surfaces a faithful 22P03 rather than a "
        "generic internal error, but the type itself is not implemented."
    ),
    "box2d": (
        "PostGIS BOX2D — an extension type outside SecantusDB's core-PostgreSQL "
        "SQL scope. The ``::BOX2D`` cast falls through to a text passthrough, so "
        "the binary result is the text form rather than PostGIS's four-float8 "
        "box encoding. Out of scope, like GEOMETRY (see ``spatial``)."
    ),
    "pgvector": (
        "The pgvector VECTOR type — an extension outside SecantusDB's "
        "core-PostgreSQL SQL scope. A ``VECTOR`` column is rejected with a "
        "faithful 0A000 (unsupported column type), which is correct emulation "
        "of a server without the extension installed, but the corpus expects a "
        "working vector type."
    ),
}
