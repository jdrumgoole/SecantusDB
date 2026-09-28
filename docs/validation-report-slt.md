# sqllogictest conformance report

SecantusDB (Python server) 0.6.0b17 · corpus `gregrahn/sqllogictest` @ `c67f97bf3ca7` · sqllogictest-rs over pgwire · 2026-09-28

**52/60 files pass end-to-end** (8 expected divergences, 0 unexpected failures).

Regenerate with `uv run python -m invoke validate-slt`.

| lane | file | result | seconds |
|---|---|---|---:|
| postgres | `evidence/in1.test` | pass | 0.0 |
| postgres | `evidence/in2.test` | pass | 0.1 |
| postgres | `evidence/slt_lang_aggfunc.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_createtrigger.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_createview.test` | expected divergence | 0.03 |
| postgres | `evidence/slt_lang_dropindex.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_droptable.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_droptrigger.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_dropview.test` | pass | 0.03 |
| postgres | `evidence/slt_lang_reindex.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_replace.test` | pass | 0.0 |
| postgres | `evidence/slt_lang_update.test` | pass | 0.06 |
| postgres | `index/orderby/10/slt_good_0.test` | pass | 49.28 |
| postgres | `index/between/1/slt_good_0.test` | pass | 121.3 |
| postgres | `index/commute/10/slt_good_0.test` | pass | 46.97 |
| postgres | `index/delete/1/slt_good_0.test` | pass | 27.79 |
| postgres | `index/in/10/slt_good_0.test` | pass | 125.76 |
| postgres | `random/aggregates/slt_good_0.test` | expected divergence | 5.67 |
| postgres | `random/aggregates/slt_good_1.test` | pass | 21.56 |
| postgres | `random/aggregates/slt_good_10.test` | pass | 22.33 |
| postgres | `random/expr/slt_good_0.test` | expected divergence | 10.95 |
| postgres | `random/expr/slt_good_1.test` | pass | 7.89 |
| postgres | `random/expr/slt_good_10.test` | pass | 13.6 |
| postgres | `random/groupby/slt_good_0.test` | pass | 20.1 |
| postgres | `random/groupby/slt_good_1.test` | pass | 20.02 |
| postgres | `random/select/slt_good_0.test` | expected divergence | 15.9 |
| postgres | `random/select/slt_good_1.test` | pass | 23.44 |
| postgres | `select1.test` | pass | 26.59 |
| postgres | `select2.test` | pass | 15.65 |
| postgres | `select3.test` | pass | 57.19 |
| postgres-extended | `evidence/in1.test` | pass | 0.0 |
| postgres-extended | `evidence/in2.test` | pass | 0.18 |
| postgres-extended | `evidence/slt_lang_aggfunc.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_createtrigger.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_createview.test` | expected divergence | 0.04 |
| postgres-extended | `evidence/slt_lang_dropindex.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_droptable.test` | pass | 0.03 |
| postgres-extended | `evidence/slt_lang_droptrigger.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_dropview.test` | pass | 0.04 |
| postgres-extended | `evidence/slt_lang_reindex.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_replace.test` | pass | 0.0 |
| postgres-extended | `evidence/slt_lang_update.test` | pass | 0.08 |
| postgres-extended | `index/orderby/10/slt_good_0.test` | pass | 84.27 |
| postgres-extended | `index/between/1/slt_good_0.test` | pass | 169.96 |
| postgres-extended | `index/commute/10/slt_good_0.test` | pass | 72.98 |
| postgres-extended | `index/delete/1/slt_good_0.test` | pass | 43.83 |
| postgres-extended | `index/in/10/slt_good_0.test` | pass | 169.5 |
| postgres-extended | `random/aggregates/slt_good_0.test` | expected divergence | 9.91 |
| postgres-extended | `random/aggregates/slt_good_1.test` | pass | 37.94 |
| postgres-extended | `random/aggregates/slt_good_10.test` | pass | 39.43 |
| postgres-extended | `random/expr/slt_good_0.test` | expected divergence | 16.89 |
| postgres-extended | `random/expr/slt_good_1.test` | pass | 12.47 |
| postgres-extended | `random/expr/slt_good_10.test` | pass | 20.95 |
| postgres-extended | `random/groupby/slt_good_0.test` | pass | 35.23 |
| postgres-extended | `random/groupby/slt_good_1.test` | pass | 35.54 |
| postgres-extended | `random/select/slt_good_0.test` | expected divergence | 27.93 |
| postgres-extended | `random/select/slt_good_1.test` | pass | 40.91 |
| postgres-extended | `select1.test` | pass | 30.32 |
| postgres-extended | `select2.test` | pass | 19.22 |
| postgres-extended | `select3.test` | pass | 68.82 |

## Expected divergences

- `postgres:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
- `postgres-extended:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres-extended:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres-extended:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres-extended:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
