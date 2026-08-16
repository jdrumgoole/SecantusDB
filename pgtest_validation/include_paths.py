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
    "char": (
        "char:250 pins TableOID=105 — crdb's deterministic descriptor id, "
        "with no ignore_table_oids directive on that stanza. Real PostgreSQL "
        "reports its own pg_class oid there too (installation-specific), so "
        "the stanza can't pass against any non-crdb server. Everything else "
        "in the file is green (oid-18 \"char\": casts, columns, params, "
        "1-char truncation, NULL for empty/zero-byte, binary format)."
    ),
}
