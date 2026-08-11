### Repeated SQL statements skip the parser

`planner.parse` now caches parsed statements by text, handing out fresh
copies of the cached trees (`Expression.copy()` measures 3–4× cheaper
than a parse, which profiled at ~29% of embedded statement time).
Entries are cached on second sight — the first occurrence only leaves a
marker — so workloads of mostly-unique statements (sqllogictest's
corpus, inline-literal DML) pay nothing beyond a dict probe, while
repeated text (per-connection re-parse of prepared statements, fixture
DDL repeated across thousands of tests) hits from the second occurrence
on: +26% embedded statement throughput on repeated-text workloads,
no measurable cost on unique-text ones. The cached trees never leave
the cache uncopied, so downstream mutation cannot poison them — pinned
by `tests/test_sql_parse_cache.py` alongside second-sight, eviction,
and error-path semantics.
