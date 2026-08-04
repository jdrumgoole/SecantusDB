# sqllogictest conformance report

SecantusDB (Python server) 0.6.0b9 · corpus `gregrahn/sqllogictest` @ `c67f97bf3ca7` · sqllogictest-rs over pgwire · 2026-08-04

**52/60 files pass end-to-end** (8 expected divergences, 0 unexpected failures).

Regenerate with `uv run python -m invoke validate-slt`.

| lane | file | result | seconds |
|---|---|---|---:|
| postgres | `evidence/in1.test` | pass | 0.0 |
| postgres | `evidence/in2.test` | pass | 0.11 |
| postgres | `evidence/slt_lang_aggfunc.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_createtrigger.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_createview.test` | expected divergence | 0.02 |
| postgres | `evidence/slt_lang_dropindex.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_droptable.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_droptrigger.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_dropview.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_reindex.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_replace.test` | pass | 0.0 |
| postgres | `evidence/slt_lang_update.test` | pass | 0.04 |
| postgres | `index/orderby/10/slt_good_0.test` | pass | 21.99 |
| postgres | `index/between/1/slt_good_0.test` | pass | 56.04 |
| postgres | `index/commute/10/slt_good_0.test` | pass | 25.28 |
| postgres | `index/delete/1/slt_good_0.test` | pass | 17.39 |
| postgres | `index/in/10/slt_good_0.test` | pass | 61.52 |
| postgres | `random/aggregates/slt_good_0.test` | expected divergence | 3.06 |
| postgres | `random/aggregates/slt_good_1.test` | pass | 11.72 |
| postgres | `random/aggregates/slt_good_10.test` | pass | 11.99 |
| postgres | `random/expr/slt_good_0.test` | expected divergence | 6.13 |
| postgres | `random/expr/slt_good_1.test` | pass | 4.89 |
| postgres | `random/expr/slt_good_10.test` | pass | 7.63 |
| postgres | `random/groupby/slt_good_0.test` | pass | 12.62 |
| postgres | `random/groupby/slt_good_1.test` | pass | 12.42 |
| postgres | `random/select/slt_good_0.test` | expected divergence | 9.22 |
| postgres | `random/select/slt_good_1.test` | pass | 13.46 |
| postgres | `select1.test` | pass | 16.11 |
| postgres | `select2.test` | pass | 9.46 |
| postgres | `select3.test` | pass | 33.74 |
| postgres-extended | `evidence/in1.test` | pass | 0.0 |
| postgres-extended | `evidence/in2.test` | pass | 4.19 |
| postgres-extended | `evidence/slt_lang_aggfunc.test` | pass | 0.45 |
| postgres-extended | `evidence/slt_lang_createtrigger.test` | pass | 0.45 |
| postgres-extended | `evidence/slt_lang_createview.test` | expected divergence | 0.87 |
| postgres-extended | `evidence/slt_lang_dropindex.test` | pass | 0.7 |
| postgres-extended | `evidence/slt_lang_droptable.test` | pass | 1.03 |
| postgres-extended | `evidence/slt_lang_droptrigger.test` | pass | 0.46 |
| postgres-extended | `evidence/slt_lang_dropview.test` | pass | 1.11 |
| postgres-extended | `evidence/slt_lang_reindex.test` | pass | 0.45 |
| postgres-extended | `evidence/slt_lang_replace.test` | pass | 0.0 |
| postgres-extended | `evidence/slt_lang_update.test` | pass | 2.26 |
| postgres-extended | `index/orderby/10/slt_good_0.test` | pass | 824.44 |
| postgres-extended | `index/between/1/slt_good_0.test` | pass | 822.17 |
| postgres-extended | `index/commute/10/slt_good_0.test` | pass | 822.88 |
| postgres-extended | `index/delete/1/slt_good_0.test` | pass | 894.46 |
| postgres-extended | `index/in/10/slt_good_0.test` | pass | 823.08 |
| postgres-extended | `random/aggregates/slt_good_0.test` | expected divergence | 212.12 |
| postgres-extended | `random/aggregates/slt_good_1.test` | pass | 821.08 |
| postgres-extended | `random/aggregates/slt_good_10.test` | pass | 821.08 |
| postgres-extended | `random/expr/slt_good_0.test` | expected divergence | 708.58 |
| postgres-extended | `random/expr/slt_good_1.test` | pass | 821.07 |
| postgres-extended | `random/expr/slt_good_10.test` | pass | 821.09 |
| postgres-extended | `random/groupby/slt_good_0.test` | pass | 821.07 |
| postgres-extended | `random/groupby/slt_good_1.test` | pass | 821.06 |
| postgres-extended | `random/select/slt_good_0.test` | expected divergence | 560.99 |
| postgres-extended | `random/select/slt_good_1.test` | pass | 821.09 |
| postgres-extended | `select1.test` | pass | 85.78 |
| postgres-extended | `select2.test` | pass | 84.59 |
| postgres-extended | `select3.test` | pass | 275.31 |

## Expected divergences

- `postgres:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
- `postgres-extended:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres-extended:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres-extended:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres-extended:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
