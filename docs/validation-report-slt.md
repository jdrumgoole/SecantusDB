# sqllogictest conformance report

SecantusDB (Python server) 0.7.0b3 · corpus `gregrahn/sqllogictest` @ `c67f97bf3ca7` · sqllogictest-rs over pgwire · 2026-10-15

**52/60 files pass end-to-end** (8 expected divergences, 0 unexpected failures).

Regenerate with `uv run python -m invoke validate-slt`.

| lane | file | result | seconds |
|---|---|---|---:|
| postgres | `evidence/in1.test` | pass | 0.0 |
| postgres | `evidence/in2.test` | pass | 0.09 |
| postgres | `evidence/slt_lang_aggfunc.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_createtrigger.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_createview.test` | expected divergence | 0.02 |
| postgres | `evidence/slt_lang_dropindex.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_droptable.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_droptrigger.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_dropview.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_reindex.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_replace.test` | pass | 0.0 |
| postgres | `evidence/slt_lang_update.test` | pass | 0.05 |
| postgres | `index/orderby/10/slt_good_0.test` | pass | 41.36 |
| postgres | `index/between/1/slt_good_0.test` | pass | 106.76 |
| postgres | `index/commute/10/slt_good_0.test` | pass | 39.19 |
| postgres | `index/delete/1/slt_good_0.test` | pass | 26.78 |
| postgres | `index/in/10/slt_good_0.test` | pass | 109.19 |
| postgres | `random/aggregates/slt_good_0.test` | expected divergence | 4.58 |
| postgres | `random/aggregates/slt_good_1.test` | pass | 17.29 |
| postgres | `random/aggregates/slt_good_10.test` | pass | 17.9 |
| postgres | `random/expr/slt_good_0.test` | expected divergence | 8.61 |
| postgres | `random/expr/slt_good_1.test` | pass | 5.94 |
| postgres | `random/expr/slt_good_10.test` | pass | 10.68 |
| postgres | `random/groupby/slt_good_0.test` | pass | 16.4 |
| postgres | `random/groupby/slt_good_1.test` | pass | 16.46 |
| postgres | `random/select/slt_good_0.test` | expected divergence | 12.95 |
| postgres | `random/select/slt_good_1.test` | pass | 18.95 |
| postgres | `select1.test` | pass | 22.86 |
| postgres | `select2.test` | pass | 13.21 |
| postgres | `select3.test` | pass | 48.27 |
| postgres-extended | `evidence/in1.test` | pass | 0.0 |
| postgres-extended | `evidence/in2.test` | pass | 0.14 |
| postgres-extended | `evidence/slt_lang_aggfunc.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_createtrigger.test` | pass | 0.01 |
| postgres-extended | `evidence/slt_lang_createview.test` | expected divergence | 0.03 |
| postgres-extended | `evidence/slt_lang_dropindex.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_droptable.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_droptrigger.test` | pass | 0.01 |
| postgres-extended | `evidence/slt_lang_dropview.test` | pass | 0.03 |
| postgres-extended | `evidence/slt_lang_reindex.test` | pass | 0.01 |
| postgres-extended | `evidence/slt_lang_replace.test` | pass | 0.0 |
| postgres-extended | `evidence/slt_lang_update.test` | pass | 0.07 |
| postgres-extended | `index/orderby/10/slt_good_0.test` | pass | 68.49 |
| postgres-extended | `index/between/1/slt_good_0.test` | pass | 138.57 |
| postgres-extended | `index/commute/10/slt_good_0.test` | pass | 59.16 |
| postgres-extended | `index/delete/1/slt_good_0.test` | pass | 39.41 |
| postgres-extended | `index/in/10/slt_good_0.test` | pass | 139.63 |
| postgres-extended | `random/aggregates/slt_good_0.test` | expected divergence | 7.96 |
| postgres-extended | `random/aggregates/slt_good_1.test` | pass | 30.14 |
| postgres-extended | `random/aggregates/slt_good_10.test` | pass | 30.81 |
| postgres-extended | `random/expr/slt_good_0.test` | expected divergence | 13.49 |
| postgres-extended | `random/expr/slt_good_1.test` | pass | 9.75 |
| postgres-extended | `random/expr/slt_good_10.test` | pass | 16.61 |
| postgres-extended | `random/groupby/slt_good_0.test` | pass | 28.04 |
| postgres-extended | `random/groupby/slt_good_1.test` | pass | 28.37 |
| postgres-extended | `random/select/slt_good_0.test` | expected divergence | 22.09 |
| postgres-extended | `random/select/slt_good_1.test` | pass | 32.12 |
| postgres-extended | `select1.test` | pass | 25.77 |
| postgres-extended | `select2.test` | pass | 15.95 |
| postgres-extended | `select3.test` | pass | 57.54 |

## Expected divergences

- `postgres:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
- `postgres-extended:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres-extended:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres-extended:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres-extended:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
