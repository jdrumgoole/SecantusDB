# sqllogictest conformance report

SecantusDB (Python server) 0.7.0b2 · corpus `gregrahn/sqllogictest` @ `c67f97bf3ca7` · sqllogictest-rs over pgwire · 2026-10-12

**52/60 files pass end-to-end** (8 expected divergences, 0 unexpected failures).

Regenerate with `uv run python -m invoke validate-slt`.

| lane | file | result | seconds |
|---|---|---|---:|
| postgres | `evidence/in1.test` | pass | 0.0 |
| postgres | `evidence/in2.test` | pass | 0.13 |
| postgres | `evidence/slt_lang_aggfunc.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_createtrigger.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_createview.test` | expected divergence | 0.03 |
| postgres | `evidence/slt_lang_dropindex.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_droptable.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_droptrigger.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_dropview.test` | pass | 0.03 |
| postgres | `evidence/slt_lang_reindex.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_replace.test` | pass | 0.0 |
| postgres | `evidence/slt_lang_update.test` | pass | 0.07 |
| postgres | `index/orderby/10/slt_good_0.test` | pass | 57.51 |
| postgres | `index/between/1/slt_good_0.test` | pass | 144.84 |
| postgres | `index/commute/10/slt_good_0.test` | pass | 56.87 |
| postgres | `index/delete/1/slt_good_0.test` | pass | 38.03 |
| postgres | `index/in/10/slt_good_0.test` | pass | 148.33 |
| postgres | `random/aggregates/slt_good_0.test` | expected divergence | 6.61 |
| postgres | `random/aggregates/slt_good_1.test` | pass | 25.09 |
| postgres | `random/aggregates/slt_good_10.test` | pass | 26.48 |
| postgres | `random/expr/slt_good_0.test` | expected divergence | 12.26 |
| postgres | `random/expr/slt_good_1.test` | pass | 8.99 |
| postgres | `random/expr/slt_good_10.test` | pass | 15.17 |
| postgres | `random/groupby/slt_good_0.test` | pass | 23.26 |
| postgres | `random/groupby/slt_good_1.test` | pass | 23.14 |
| postgres | `random/select/slt_good_0.test` | expected divergence | 18.61 |
| postgres | `random/select/slt_good_1.test` | pass | 27.16 |
| postgres | `select1.test` | pass | 32.36 |
| postgres | `select2.test` | pass | 18.52 |
| postgres | `select3.test` | pass | 68.25 |
| postgres-extended | `evidence/in1.test` | pass | 0.0 |
| postgres-extended | `evidence/in2.test` | pass | 0.21 |
| postgres-extended | `evidence/slt_lang_aggfunc.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_createtrigger.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_createview.test` | expected divergence | 0.04 |
| postgres-extended | `evidence/slt_lang_dropindex.test` | pass | 0.03 |
| postgres-extended | `evidence/slt_lang_droptable.test` | pass | 0.03 |
| postgres-extended | `evidence/slt_lang_droptrigger.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_dropview.test` | pass | 0.04 |
| postgres-extended | `evidence/slt_lang_reindex.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_replace.test` | pass | 0.0 |
| postgres-extended | `evidence/slt_lang_update.test` | pass | 0.1 |
| postgres-extended | `index/orderby/10/slt_good_0.test` | pass | 95.5 |
| postgres-extended | `index/between/1/slt_good_0.test` | pass | 197.12 |
| postgres-extended | `index/commute/10/slt_good_0.test` | pass | 85.17 |
| postgres-extended | `index/delete/1/slt_good_0.test` | pass | 56.72 |
| postgres-extended | `index/in/10/slt_good_0.test` | pass | 203.38 |
| postgres-extended | `random/aggregates/slt_good_0.test` | expected divergence | 11.73 |
| postgres-extended | `random/aggregates/slt_good_1.test` | pass | 44.38 |
| postgres-extended | `random/aggregates/slt_good_10.test` | pass | 46.28 |
| postgres-extended | `random/expr/slt_good_0.test` | expected divergence | 19.51 |
| postgres-extended | `random/expr/slt_good_1.test` | pass | 14.73 |
| postgres-extended | `random/expr/slt_good_10.test` | pass | 24.3 |
| postgres-extended | `random/groupby/slt_good_0.test` | pass | 41.03 |
| postgres-extended | `random/groupby/slt_good_1.test` | pass | 41.34 |
| postgres-extended | `random/select/slt_good_0.test` | expected divergence | 32.59 |
| postgres-extended | `random/select/slt_good_1.test` | pass | 48.09 |
| postgres-extended | `select1.test` | pass | 36.88 |
| postgres-extended | `select2.test` | pass | 22.59 |
| postgres-extended | `select3.test` | pass | 83.74 |

## Expected divergences

- `postgres:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
- `postgres-extended:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres-extended:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres-extended:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres-extended:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
