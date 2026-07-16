# sqllogictest conformance report

SecantusDB (Python server) 0.5.4b234 · corpus `gregrahn/sqllogictest` @ `c67f97bf3ca7` · sqllogictest-rs over pgwire · 2026-07-14

**26/30 files pass end-to-end** (4 expected divergences, 0 unexpected failures).

Regenerate with `uv run python -m invoke validate-slt`.

| file | result | seconds |
|---|---|---:|
| `evidence/in1.test` | pass | 0.02 |
| `evidence/in2.test` | pass | 0.05 |
| `evidence/slt_lang_aggfunc.test` | pass | 0.02 |
| `evidence/slt_lang_createtrigger.test` | pass | 0.01 |
| `evidence/slt_lang_createview.test` | expected divergence | 0.02 |
| `evidence/slt_lang_dropindex.test` | pass | 0.02 |
| `evidence/slt_lang_droptable.test` | pass | 0.02 |
| `evidence/slt_lang_droptrigger.test` | pass | 0.02 |
| `evidence/slt_lang_dropview.test` | pass | 0.02 |
| `evidence/slt_lang_reindex.test` | pass | 0.01 |
| `evidence/slt_lang_replace.test` | pass | 0.01 |
| `evidence/slt_lang_update.test` | pass | 0.04 |
| `index/orderby/10/slt_good_0.test` | pass | 14.44 |
| `index/between/1/slt_good_0.test` | pass | 31.93 |
| `index/commute/10/slt_good_0.test` | pass | 12.95 |
| `index/delete/1/slt_good_0.test` | pass | 9.36 |
| `index/in/10/slt_good_0.test` | pass | 34.81 |
| `random/aggregates/slt_good_0.test` | expected divergence | 1.83 |
| `random/aggregates/slt_good_1.test` | pass | 6.88 |
| `random/aggregates/slt_good_10.test` | pass | 7.11 |
| `random/expr/slt_good_0.test` | expected divergence | 3.82 |
| `random/expr/slt_good_1.test` | pass | 2.99 |
| `random/expr/slt_good_10.test` | pass | 4.74 |
| `random/groupby/slt_good_0.test` | pass | 7.63 |
| `random/groupby/slt_good_1.test` | pass | 7.54 |
| `random/select/slt_good_0.test` | expected divergence | 6.33 |
| `random/select/slt_good_1.test` | pass | 9.4 |
| `select1.test` | pass | 17.08 |
| `select2.test` | pass | 9.6 |
| `select3.test` | pass | 34.96 |

## Expected divergences

- `evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
