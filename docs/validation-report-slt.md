# sqllogictest conformance report

SecantusDB (Python server) 0.6.0b16 · corpus `gregrahn/sqllogictest` @ `c67f97bf3ca7` · sqllogictest-rs over pgwire · 2026-08-30

**52/60 files pass end-to-end** (8 expected divergences, 0 unexpected failures).

Regenerate with `uv run python -m invoke validate-slt`.

| lane | file | result | seconds |
|---|---|---|---:|
| postgres | `evidence/in1.test` | pass | 0.02 |
| postgres | `evidence/in2.test` | pass | 0.09 |
| postgres | `evidence/slt_lang_aggfunc.test` | pass | 0.04 |
| postgres | `evidence/slt_lang_createtrigger.test` | pass | 0.04 |
| postgres | `evidence/slt_lang_createview.test` | expected divergence | 0.05 |
| postgres | `evidence/slt_lang_dropindex.test` | pass | 0.04 |
| postgres | `evidence/slt_lang_droptable.test` | pass | 0.04 |
| postgres | `evidence/slt_lang_droptrigger.test` | pass | 0.04 |
| postgres | `evidence/slt_lang_dropview.test` | pass | 0.06 |
| postgres | `evidence/slt_lang_reindex.test` | pass | 0.04 |
| postgres | `evidence/slt_lang_replace.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_update.test` | pass | 0.06 |
| postgres | `index/orderby/10/slt_good_0.test` | pass | 24.45 |
| postgres | `index/between/1/slt_good_0.test` | pass | 54.77 |
| postgres | `index/commute/10/slt_good_0.test` | pass | 23.21 |
| postgres | `index/delete/1/slt_good_0.test` | pass | 14.2 |
| postgres | `index/in/10/slt_good_0.test` | pass | 58.45 |
| postgres | `random/aggregates/slt_good_0.test` | expected divergence | 2.96 |
| postgres | `random/aggregates/slt_good_1.test` | pass | 11.12 |
| postgres | `random/aggregates/slt_good_10.test` | pass | 11.43 |
| postgres | `random/expr/slt_good_0.test` | expected divergence | 5.36 |
| postgres | `random/expr/slt_good_1.test` | pass | 3.94 |
| postgres | `random/expr/slt_good_10.test` | pass | 6.62 |
| postgres | `random/groupby/slt_good_0.test` | pass | 10.43 |
| postgres | `random/groupby/slt_good_1.test` | pass | 10.45 |
| postgres | `random/select/slt_good_0.test` | expected divergence | 8.69 |
| postgres | `random/select/slt_good_1.test` | pass | 12.71 |
| postgres | `select1.test` | pass | 17.27 |
| postgres | `select2.test` | pass | 9.78 |
| postgres | `select3.test` | pass | 35.85 |
| postgres-extended | `evidence/in1.test` | pass | 0.01 |
| postgres-extended | `evidence/in2.test` | pass | 0.12 |
| postgres-extended | `evidence/slt_lang_aggfunc.test` | pass | 0.05 |
| postgres-extended | `evidence/slt_lang_createtrigger.test` | pass | 0.04 |
| postgres-extended | `evidence/slt_lang_createview.test` | expected divergence | 0.06 |
| postgres-extended | `evidence/slt_lang_dropindex.test` | pass | 0.04 |
| postgres-extended | `evidence/slt_lang_droptable.test` | pass | 0.05 |
| postgres-extended | `evidence/slt_lang_droptrigger.test` | pass | 0.04 |
| postgres-extended | `evidence/slt_lang_dropview.test` | pass | 0.06 |
| postgres-extended | `evidence/slt_lang_reindex.test` | pass | 0.04 |
| postgres-extended | `evidence/slt_lang_replace.test` | pass | 0.01 |
| postgres-extended | `evidence/slt_lang_update.test` | pass | 0.09 |
| postgres-extended | `index/orderby/10/slt_good_0.test` | pass | 41.09 |
| postgres-extended | `index/between/1/slt_good_0.test` | pass | 74.92 |
| postgres-extended | `index/commute/10/slt_good_0.test` | pass | 36.5 |
| postgres-extended | `index/delete/1/slt_good_0.test` | pass | 22.31 |
| postgres-extended | `index/in/10/slt_good_0.test` | pass | 77.3 |
| postgres-extended | `random/aggregates/slt_good_0.test` | expected divergence | 5.0 |
| postgres-extended | `random/aggregates/slt_good_1.test` | pass | 19.02 |
| postgres-extended | `random/aggregates/slt_good_10.test` | pass | 19.4 |
| postgres-extended | `random/expr/slt_good_0.test` | expected divergence | 8.06 |
| postgres-extended | `random/expr/slt_good_1.test` | pass | 5.75 |
| postgres-extended | `random/expr/slt_good_10.test` | pass | 9.86 |
| postgres-extended | `random/groupby/slt_good_0.test` | pass | 17.79 |
| postgres-extended | `random/groupby/slt_good_1.test` | pass | 17.63 |
| postgres-extended | `random/select/slt_good_0.test` | expected divergence | 14.6 |
| postgres-extended | `random/select/slt_good_1.test` | pass | 21.4 |
| postgres-extended | `select1.test` | pass | 19.29 |
| postgres-extended | `select2.test` | pass | 11.6 |
| postgres-extended | `select3.test` | pass | 42.58 |

## Expected divergences

- `postgres:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
- `postgres-extended:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres-extended:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres-extended:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres-extended:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
