# sqllogictest conformance report

SecantusDB (Python server) 0.6.0b9 · corpus `gregrahn/sqllogictest` @ `c67f97bf3ca7` · sqllogictest-rs over pgwire · 2026-08-11

**52/60 files pass end-to-end** (8 expected divergences, 0 unexpected failures).

Regenerate with `uv run python -m invoke validate-slt`.

| lane | file | result | seconds |
|---|---|---|---:|
| postgres | `evidence/in1.test` | pass | 0.0 |
| postgres | `evidence/in2.test` | pass | 0.08 |
| postgres | `evidence/slt_lang_aggfunc.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_createtrigger.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_createview.test` | expected divergence | 0.03 |
| postgres | `evidence/slt_lang_dropindex.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_droptable.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_droptrigger.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_dropview.test` | pass | 0.03 |
| postgres | `evidence/slt_lang_reindex.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_replace.test` | pass | 0.0 |
| postgres | `evidence/slt_lang_update.test` | pass | 0.05 |
| postgres | `index/orderby/10/slt_good_0.test` | pass | 32.65 |
| postgres | `index/between/1/slt_good_0.test` | pass | 94.28 |
| postgres | `index/commute/10/slt_good_0.test` | pass | 35.79 |
| postgres | `index/delete/1/slt_good_0.test` | pass | 21.55 |
| postgres | `index/in/10/slt_good_0.test` | pass | 94.02 |
| postgres | `random/aggregates/slt_good_0.test` | expected divergence | 3.9 |
| postgres | `random/aggregates/slt_good_1.test` | pass | 14.87 |
| postgres | `random/aggregates/slt_good_10.test` | pass | 15.59 |
| postgres | `random/expr/slt_good_0.test` | expected divergence | 8.64 |
| postgres | `random/expr/slt_good_1.test` | pass | 6.76 |
| postgres | `random/expr/slt_good_10.test` | pass | 10.69 |
| postgres | `random/groupby/slt_good_0.test` | pass | 16.21 |
| postgres | `random/groupby/slt_good_1.test` | pass | 15.98 |
| postgres | `random/select/slt_good_0.test` | expected divergence | 12.0 |
| postgres | `random/select/slt_good_1.test` | pass | 17.73 |
| postgres | `select1.test` | pass | 22.71 |
| postgres | `select2.test` | pass | 13.03 |
| postgres | `select3.test` | pass | 47.69 |
| postgres-extended | `evidence/in1.test` | pass | 0.0 |
| postgres-extended | `evidence/in2.test` | pass | 0.12 |
| postgres-extended | `evidence/slt_lang_aggfunc.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_createtrigger.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_createview.test` | expected divergence | 0.03 |
| postgres-extended | `evidence/slt_lang_dropindex.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_droptable.test` | pass | 0.03 |
| postgres-extended | `evidence/slt_lang_droptrigger.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_dropview.test` | pass | 0.03 |
| postgres-extended | `evidence/slt_lang_reindex.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_replace.test` | pass | 0.0 |
| postgres-extended | `evidence/slt_lang_update.test` | pass | 0.07 |
| postgres-extended | `index/orderby/10/slt_good_0.test` | pass | 49.87 |
| postgres-extended | `index/between/1/slt_good_0.test` | pass | 114.79 |
| postgres-extended | `index/commute/10/slt_good_0.test` | pass | 53.99 |
| postgres-extended | `index/delete/1/slt_good_0.test` | pass | 32.81 |
| postgres-extended | `index/in/10/slt_good_0.test` | pass | 118.15 |
| postgres-extended | `random/aggregates/slt_good_0.test` | expected divergence | 6.8 |
| postgres-extended | `random/aggregates/slt_good_1.test` | pass | 25.91 |
| postgres-extended | `random/aggregates/slt_good_10.test` | pass | 26.65 |
| postgres-extended | `random/expr/slt_good_0.test` | expected divergence | 13.94 |
| postgres-extended | `random/expr/slt_good_1.test` | pass | 11.89 |
| postgres-extended | `random/expr/slt_good_10.test` | pass | 17.25 |
| postgres-extended | `random/groupby/slt_good_0.test` | pass | 27.9 |
| postgres-extended | `random/groupby/slt_good_1.test` | pass | 27.57 |
| postgres-extended | `random/select/slt_good_0.test` | expected divergence | 19.76 |
| postgres-extended | `random/select/slt_good_1.test` | pass | 29.46 |
| postgres-extended | `select1.test` | pass | 25.51 |
| postgres-extended | `select2.test` | pass | 15.24 |
| postgres-extended | `select3.test` | pass | 55.58 |

## Expected divergences

- `postgres:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
- `postgres-extended:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres-extended:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres-extended:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres-extended:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
