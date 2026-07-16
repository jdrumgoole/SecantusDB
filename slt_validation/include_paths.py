"""Which corpus files the sqllogictest gauge runs, and the expected divergences.

Same include/deselect model as the driver gauges: the vendored corpus is NEVER
modified — divergence lives here. Grow ``INCLUDE`` as conformance grows; an
``EXPECTED_DIVERGENCES`` entry needs a one-line reason and represents a
documented PG-vs-SQLite (or runner) incompatibility, not a SecantusDB gap —
the gauge treats those files' failures as expected and stays green.
"""

# Corpus files (relative to vendor/sqllogictest/test) the gauge measures. The
# curated set from the 2026-07-14 sweeps: all of evidence/, the canonical
# select files, and samples from each random/ and index/ family. Grow toward
# the full 622-file corpus as runtime budget allows (each file runs against a
# fresh daemon; the current set is ~3 minutes).
INCLUDE = [
    "evidence/in1.test",
    "evidence/in2.test",
    "evidence/slt_lang_aggfunc.test",
    "evidence/slt_lang_createtrigger.test",
    "evidence/slt_lang_createview.test",
    "evidence/slt_lang_dropindex.test",
    "evidence/slt_lang_droptable.test",
    "evidence/slt_lang_droptrigger.test",
    "evidence/slt_lang_dropview.test",
    "evidence/slt_lang_reindex.test",
    "evidence/slt_lang_replace.test",
    "evidence/slt_lang_update.test",
    "index/orderby/10/slt_good_0.test",
    "index/between/1/slt_good_0.test",
    "index/commute/10/slt_good_0.test",
    "index/delete/1/slt_good_0.test",
    "index/in/10/slt_good_0.test",
    "random/aggregates/slt_good_0.test",
    "random/aggregates/slt_good_1.test",
    "random/aggregates/slt_good_10.test",
    "random/expr/slt_good_0.test",
    "random/expr/slt_good_1.test",
    "random/expr/slt_good_10.test",
    "random/groupby/slt_good_0.test",
    "random/groupby/slt_good_1.test",
    "random/select/slt_good_0.test",
    "random/select/slt_good_1.test",
    "select1.test",
    "select2.test",
    "select3.test",
]

# Files whose failure is a documented divergence, not a SecantusDB gap. Keyed
# by the include path; the value is the reason shown in the report. A file
# listed here that PASSES is reported loudly (the divergence resolved — move
# it back to plain INCLUDE).
EXPECTED_DIVERGENCES = {
    "evidence/slt_lang_createview.test": (
        "corpus expects SQLite read-only views; real Postgres auto-updates "
        "simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)"
    ),
    "random/aggregates/slt_good_0.test": (
        "corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise "
        "SQLSTATE 22012 (~22k records in)"
    ),
    "random/expr/slt_good_0.test": (
        "corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise "
        "SQLSTATE 22012 (~75k records in)"
    ),
    "random/select/slt_good_0.test": (
        "the corpus expects the RUNNER to cast REAL results to int per the "
        "'query I' type string; sqllogictest-rs doesn't (~52k records in)"
    ),
}
