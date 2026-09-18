# sqllogictest conformance report

SecantusDB (Python server) 0.6.0b16 · corpus `gregrahn/sqllogictest` @ `c67f97bf3ca7` · sqllogictest-rs over pgwire · 2026-09-18

**52/60 files pass end-to-end** (8 expected divergences, 0 unexpected failures).

Regenerate with `uv run python -m invoke validate-slt`.

| lane | file | result | seconds |
|---|---|---|---:|
| postgres | `evidence/in1.test` | pass | 0.0 |
| postgres | `evidence/in2.test` | pass | 0.11 |
| postgres | `evidence/slt_lang_aggfunc.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_createtrigger.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_createview.test` | expected divergence | 0.03 |
| postgres | `evidence/slt_lang_dropindex.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_droptable.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_droptrigger.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_dropview.test` | pass | 0.03 |
| postgres | `evidence/slt_lang_reindex.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_replace.test` | pass | 0.0 |
| postgres | `evidence/slt_lang_update.test` | pass | 0.06 |
| postgres | `index/orderby/10/slt_good_0.test` | pass | 53.47 |
| postgres | `index/between/1/slt_good_0.test` | pass | 137.9 |
| postgres | `index/commute/10/slt_good_0.test` | pass | 50.53 |
| postgres | `index/delete/1/slt_good_0.test` | pass | 29.22 |
| postgres | `index/in/10/slt_good_0.test` | pass | 140.61 |
| postgres | `random/aggregates/slt_good_0.test` | expected divergence | 6.01 |
| postgres | `random/aggregates/slt_good_1.test` | pass | 22.73 |
| postgres | `random/aggregates/slt_good_10.test` | pass | 23.58 |
| postgres | `random/expr/slt_good_0.test` | expected divergence | 11.75 |
| postgres | `random/expr/slt_good_1.test` | pass | 8.47 |
| postgres | `random/expr/slt_good_10.test` | pass | 14.59 |
| postgres | `random/groupby/slt_good_0.test` | pass | 21.2 |
| postgres | `random/groupby/slt_good_1.test` | pass | 21.21 |
| postgres | `random/select/slt_good_0.test` | expected divergence | 17.01 |
| postgres | `random/select/slt_good_1.test` | pass | 24.83 |
| postgres | `select1.test` | pass | 29.49 |
| postgres | `select2.test` | pass | 16.99 |
| postgres | `select3.test` | pass | 62.69 |
| postgres-extended | `evidence/in1.test` | pass | 0.0 |
| postgres-extended | `evidence/in2.test` | pass | 0.18 |
| postgres-extended | `evidence/slt_lang_aggfunc.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_createtrigger.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_createview.test` | expected divergence | 0.04 |
| postgres-extended | `evidence/slt_lang_dropindex.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_droptable.test` | pass | 0.03 |
| postgres-extended | `evidence/slt_lang_droptrigger.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_dropview.test` | pass | 0.05 |
| postgres-extended | `evidence/slt_lang_reindex.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_replace.test` | pass | 0.0 |
| postgres-extended | `evidence/slt_lang_update.test` | pass | 0.08 |
| postgres-extended | `index/orderby/10/slt_good_0.test` | pass | 85.17 |
| postgres-extended | `index/between/1/slt_good_0.test` | pass | 174.43 |
| postgres-extended | `index/commute/10/slt_good_0.test` | pass | 73.44 |
| postgres-extended | `index/delete/1/slt_good_0.test` | pass | 44.49 |
| postgres-extended | `index/in/10/slt_good_0.test` | pass | 176.85 |
| postgres-extended | `random/aggregates/slt_good_0.test` | expected divergence | 10.28 |
| postgres-extended | `random/aggregates/slt_good_1.test` | pass | 39.28 |
| postgres-extended | `random/aggregates/slt_good_10.test` | pass | 40.73 |
| postgres-extended | `random/expr/slt_good_0.test` | expected divergence | 18.85 |
| postgres-extended | `random/expr/slt_good_1.test` | pass | 14.6 |
| postgres-extended | `random/expr/slt_good_10.test` | pass | 23.07 |
| postgres-extended | `random/groupby/slt_good_0.test` | pass | 35.64 |
| postgres-extended | `random/groupby/slt_good_1.test` | pass | 35.99 |
| postgres-extended | `random/select/slt_good_0.test` | expected divergence | 28.63 |
| postgres-extended | `random/select/slt_good_1.test` | pass | 41.53 |
| postgres-extended | `select1.test` | pass | 32.28 |
| postgres-extended | `select2.test` | pass | 19.98 |
| postgres-extended | `select3.test` | pass | 72.86 |

## Expected divergences

- `postgres:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
- `postgres-extended:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres-extended:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres-extended:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres-extended:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
