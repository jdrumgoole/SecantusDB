# sqllogictest conformance report

SecantusDB (Python server) 0.6.0b7 · corpus `gregrahn/sqllogictest` @ `c67f97bf3ca7` · sqllogictest-rs over pgwire · 2026-07-31

**52/60 files pass end-to-end** (8 expected divergences, 0 unexpected failures).

Regenerate with `uv run python -m invoke validate-slt`.

| lane | file | result | seconds |
|---|---|---|---:|
| postgres | `evidence/in1.test` | pass | 0.01 |
| postgres | `evidence/in2.test` | pass | 0.07 |
| postgres | `evidence/slt_lang_aggfunc.test` | pass | 0.04 |
| postgres | `evidence/slt_lang_createtrigger.test` | pass | 0.04 |
| postgres | `evidence/slt_lang_createview.test` | expected divergence | 0.06 |
| postgres | `evidence/slt_lang_dropindex.test` | pass | 0.03 |
| postgres | `evidence/slt_lang_droptable.test` | pass | 0.04 |
| postgres | `evidence/slt_lang_droptrigger.test` | pass | 0.04 |
| postgres | `evidence/slt_lang_dropview.test` | pass | 0.05 |
| postgres | `evidence/slt_lang_reindex.test` | pass | 0.04 |
| postgres | `evidence/slt_lang_replace.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_update.test` | pass | 0.06 |
| postgres | `index/orderby/10/slt_good_0.test` | pass | 14.83 |
| postgres | `index/between/1/slt_good_0.test` | pass | 34.89 |
| postgres | `index/commute/10/slt_good_0.test` | pass | 14.25 |
| postgres | `index/delete/1/slt_good_0.test` | pass | 10.08 |
| postgres | `index/in/10/slt_good_0.test` | pass | 37.33 |
| postgres | `random/aggregates/slt_good_0.test` | expected divergence | 2.06 |
| postgres | `random/aggregates/slt_good_1.test` | pass | 7.62 |
| postgres | `random/aggregates/slt_good_10.test` | pass | 7.79 |
| postgres | `random/expr/slt_good_0.test` | expected divergence | 4.29 |
| postgres | `random/expr/slt_good_1.test` | pass | 3.29 |
| postgres | `random/expr/slt_good_10.test` | pass | 5.3 |
| postgres | `random/groupby/slt_good_0.test` | pass | 8.21 |
| postgres | `random/groupby/slt_good_1.test` | pass | 8.21 |
| postgres | `random/select/slt_good_0.test` | expected divergence | 6.8 |
| postgres | `random/select/slt_good_1.test` | pass | 9.97 |
| postgres | `select1.test` | pass | 15.44 |
| postgres | `select2.test` | pass | 8.68 |
| postgres | `select3.test` | pass | 31.78 |
| postgres-extended | `evidence/in1.test` | pass | 0.01 |
| postgres-extended | `evidence/in2.test` | pass | 0.1 |
| postgres-extended | `evidence/slt_lang_aggfunc.test` | pass | 0.04 |
| postgres-extended | `evidence/slt_lang_createtrigger.test` | pass | 0.04 |
| postgres-extended | `evidence/slt_lang_createview.test` | expected divergence | 0.06 |
| postgres-extended | `evidence/slt_lang_dropindex.test` | pass | 0.04 |
| postgres-extended | `evidence/slt_lang_droptable.test` | pass | 0.04 |
| postgres-extended | `evidence/slt_lang_droptrigger.test` | pass | 0.04 |
| postgres-extended | `evidence/slt_lang_dropview.test` | pass | 0.05 |
| postgres-extended | `evidence/slt_lang_reindex.test` | pass | 0.04 |
| postgres-extended | `evidence/slt_lang_replace.test` | pass | 0.01 |
| postgres-extended | `evidence/slt_lang_update.test` | pass | 0.07 |
| postgres-extended | `index/orderby/10/slt_good_0.test` | pass | 27.43 |
| postgres-extended | `index/between/1/slt_good_0.test` | pass | 55.96 |
| postgres-extended | `index/commute/10/slt_good_0.test` | pass | 23.12 |
| postgres-extended | `index/delete/1/slt_good_0.test` | pass | 15.43 |
| postgres-extended | `index/in/10/slt_good_0.test` | pass | 57.6 |
| postgres-extended | `random/aggregates/slt_good_0.test` | expected divergence | 3.74 |
| postgres-extended | `random/aggregates/slt_good_1.test` | pass | 14.08 |
| postgres-extended | `random/aggregates/slt_good_10.test` | pass | 14.44 |
| postgres-extended | `random/expr/slt_good_0.test` | expected divergence | 7.65 |
| postgres-extended | `random/expr/slt_good_1.test` | pass | 5.23 |
| postgres-extended | `random/expr/slt_good_10.test` | pass | 9.24 |
| postgres-extended | `random/groupby/slt_good_0.test` | pass | 15.56 |
| postgres-extended | `random/groupby/slt_good_1.test` | pass | 15.07 |
| postgres-extended | `random/select/slt_good_0.test` | expected divergence | 10.91 |
| postgres-extended | `random/select/slt_good_1.test` | pass | 16.14 |
| postgres-extended | `select1.test` | pass | 17.26 |
| postgres-extended | `select2.test` | pass | 10.31 |
| postgres-extended | `select3.test` | pass | 37.79 |

## Expected divergences

- `postgres:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
- `postgres-extended:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres-extended:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres-extended:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres-extended:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
