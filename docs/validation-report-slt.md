# sqllogictest conformance report

SecantusDB (Python server) 0.6.0b9 · corpus `gregrahn/sqllogictest` @ `c67f97bf3ca7` · sqllogictest-rs over pgwire · 2026-08-10

**52/60 files pass end-to-end** (8 expected divergences, 0 unexpected failures).

Regenerate with `uv run python -m invoke validate-slt`.

| lane | file | result | seconds |
|---|---|---|---:|
| postgres | `evidence/in1.test` | pass | 0.0 |
| postgres | `evidence/in2.test` | pass | 0.06 |
| postgres | `evidence/slt_lang_aggfunc.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_createtrigger.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_createview.test` | expected divergence | 0.02 |
| postgres | `evidence/slt_lang_dropindex.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_droptable.test` | pass | 0.42 |
| postgres | `evidence/slt_lang_droptrigger.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_dropview.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_reindex.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_replace.test` | pass | 0.0 |
| postgres | `evidence/slt_lang_update.test` | pass | 0.06 |
| postgres | `index/orderby/10/slt_good_0.test` | pass | 20.55 |
| postgres | `index/between/1/slt_good_0.test` | pass | 55.93 |
| postgres | `index/commute/10/slt_good_0.test` | pass | 23.21 |
| postgres | `index/delete/1/slt_good_0.test` | pass | 15.47 |
| postgres | `index/in/10/slt_good_0.test` | pass | 57.26 |
| postgres | `random/aggregates/slt_good_0.test` | expected divergence | 2.6 |
| postgres | `random/aggregates/slt_good_1.test` | pass | 9.91 |
| postgres | `random/aggregates/slt_good_10.test` | pass | 10.36 |
| postgres | `random/expr/slt_good_0.test` | expected divergence | 5.52 |
| postgres | `random/expr/slt_good_1.test` | pass | 4.26 |
| postgres | `random/expr/slt_good_10.test` | pass | 6.78 |
| postgres | `random/groupby/slt_good_0.test` | pass | 10.69 |
| postgres | `random/groupby/slt_good_1.test` | pass | 10.69 |
| postgres | `random/select/slt_good_0.test` | expected divergence | 8.23 |
| postgres | `random/select/slt_good_1.test` | pass | 12.0 |
| postgres | `select1.test` | pass | 16.25 |
| postgres | `select2.test` | pass | 9.22 |
| postgres | `select3.test` | pass | 33.87 |
| postgres-extended | `evidence/in1.test` | pass | 0.0 |
| postgres-extended | `evidence/in2.test` | pass | 4.19 |
| postgres-extended | `evidence/slt_lang_aggfunc.test` | pass | 0.45 |
| postgres-extended | `evidence/slt_lang_createtrigger.test` | pass | 0.56 |
| postgres-extended | `evidence/slt_lang_createview.test` | expected divergence | 0.87 |
| postgres-extended | `evidence/slt_lang_dropindex.test` | pass | 0.78 |
| postgres-extended | `evidence/slt_lang_droptable.test` | pass | 1.05 |
| postgres-extended | `evidence/slt_lang_droptrigger.test` | pass | 0.55 |
| postgres-extended | `evidence/slt_lang_dropview.test` | pass | 1.25 |
| postgres-extended | `evidence/slt_lang_reindex.test` | pass | 0.61 |
| postgres-extended | `evidence/slt_lang_replace.test` | pass | 0.0 |
| postgres-extended | `evidence/slt_lang_update.test` | pass | 2.26 |
| postgres-extended | `index/orderby/10/slt_good_0.test` | pass | 824.45 |
| postgres-extended | `index/between/1/slt_good_0.test` | pass | 822.31 |
| postgres-extended | `index/commute/10/slt_good_0.test` | pass | 823.37 |
| postgres-extended | `index/delete/1/slt_good_0.test` | pass | 894.79 |
| postgres-extended | `index/in/10/slt_good_0.test` | pass | 823.69 |
| postgres-extended | `random/aggregates/slt_good_0.test` | expected divergence | 212.28 |
| postgres-extended | `random/aggregates/slt_good_1.test` | pass | 821.12 |
| postgres-extended | `random/aggregates/slt_good_10.test` | pass | 821.09 |
| postgres-extended | `random/expr/slt_good_0.test` | expected divergence | 708.66 |
| postgres-extended | `random/expr/slt_good_1.test` | pass | 821.47 |
| postgres-extended | `random/expr/slt_good_10.test` | pass | 821.11 |
| postgres-extended | `random/groupby/slt_good_0.test` | pass | 821.09 |
| postgres-extended | `random/groupby/slt_good_1.test` | pass | 821.24 |
| postgres-extended | `random/select/slt_good_0.test` | expected divergence | 561.14 |
| postgres-extended | `random/select/slt_good_1.test` | pass | 821.23 |
| postgres-extended | `select1.test` | pass | 86.08 |
| postgres-extended | `select2.test` | pass | 84.64 |
| postgres-extended | `select3.test` | pass | 275.61 |

## Expected divergences

- `postgres:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
- `postgres-extended:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres-extended:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres-extended:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres-extended:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
