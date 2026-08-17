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
    "char": (
        "char:250 pins TableOID=105 — crdb's deterministic descriptor id, "
        "with no ignore_table_oids directive on that stanza. Real PostgreSQL "
        "reports its own pg_class oid there too (installation-specific), so "
        "the stanza can't pass against any non-crdb server. Everything else "
        "in the file is green (oid-18 \"char\": casts, columns, params, "
        "1-char truncation, NULL for empty/zero-byte, binary format)."
    ),
}
