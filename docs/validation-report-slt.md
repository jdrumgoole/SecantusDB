# sqllogictest conformance report

SecantusDB (Python server) 0.6.0b16 · corpus `gregrahn/sqllogictest` @ `c67f97bf3ca7` · sqllogictest-rs over pgwire · 2026-09-14

**52/60 files pass end-to-end** (8 expected divergences, 0 unexpected failures).

Regenerate with `uv run python -m invoke validate-slt`.

| lane | file | result | seconds |
|---|---|---|---:|
| postgres | `evidence/in1.test` | pass | 0.0 |
| postgres | `evidence/in2.test` | pass | 0.08 |
| postgres | `evidence/slt_lang_aggfunc.test` | pass | 0.07 |
| postgres | `evidence/slt_lang_createtrigger.test` | pass | 0.04 |
| postgres | `evidence/slt_lang_createview.test` | expected divergence | 0.05 |
| postgres | `evidence/slt_lang_dropindex.test` | pass | 0.12 |
| postgres | `evidence/slt_lang_droptable.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_droptrigger.test` | pass | 0.11 |
| postgres | `evidence/slt_lang_dropview.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_reindex.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_replace.test` | pass | 0.0 |
| postgres | `evidence/slt_lang_update.test` | pass | 0.07 |
| postgres | `index/orderby/10/slt_good_0.test` | pass | 40.17 |
| postgres | `index/between/1/slt_good_0.test` | pass | 107.65 |
| postgres | `index/commute/10/slt_good_0.test` | pass | 37.72 |
| postgres | `index/delete/1/slt_good_0.test` | pass | 21.67 |
| postgres | `index/in/10/slt_good_0.test` | pass | 108.38 |
| postgres | `random/aggregates/slt_good_0.test` | expected divergence | 4.33 |
| postgres | `random/aggregates/slt_good_1.test` | pass | 16.28 |
| postgres | `random/aggregates/slt_good_10.test` | pass | 16.91 |
| postgres | `random/expr/slt_good_0.test` | expected divergence | 8.56 |
| postgres | `random/expr/slt_good_1.test` | pass | 5.99 |
| postgres | `random/expr/slt_good_10.test` | pass | 10.8 |
| postgres | `random/groupby/slt_good_0.test` | pass | 15.9 |
| postgres | `random/groupby/slt_good_1.test` | pass | 16.04 |
| postgres | `random/select/slt_good_0.test` | expected divergence | 12.19 |
| postgres | `random/select/slt_good_1.test` | pass | 17.8 |
| postgres | `select1.test` | pass | 21.6 |
| postgres | `select2.test` | pass | 12.72 |
| postgres | `select3.test` | pass | 46.88 |
| postgres-extended | `evidence/in1.test` | pass | 0.0 |
| postgres-extended | `evidence/in2.test` | pass | 0.14 |
| postgres-extended | `evidence/slt_lang_aggfunc.test` | pass | 0.01 |
| postgres-extended | `evidence/slt_lang_createtrigger.test` | pass | 0.01 |
| postgres-extended | `evidence/slt_lang_createview.test` | expected divergence | 0.03 |
| postgres-extended | `evidence/slt_lang_dropindex.test` | pass | 0.03 |
| postgres-extended | `evidence/slt_lang_droptable.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_droptrigger.test` | pass | 0.01 |
| postgres-extended | `evidence/slt_lang_dropview.test` | pass | 0.08 |
| postgres-extended | `evidence/slt_lang_reindex.test` | pass | 0.01 |
| postgres-extended | `evidence/slt_lang_replace.test` | pass | 0.0 |
| postgres-extended | `evidence/slt_lang_update.test` | pass | 0.06 |
| postgres-extended | `index/orderby/10/slt_good_0.test` | pass | 63.97 |
| postgres-extended | `index/between/1/slt_good_0.test` | pass | 130.29 |
| postgres-extended | `index/commute/10/slt_good_0.test` | pass | 54.11 |
| postgres-extended | `index/delete/1/slt_good_0.test` | pass | 32.67 |
| postgres-extended | `index/in/10/slt_good_0.test` | pass | 130.19 |
| postgres-extended | `random/aggregates/slt_good_0.test` | expected divergence | 7.16 |
| postgres-extended | `random/aggregates/slt_good_1.test` | pass | 27.08 |
| postgres-extended | `random/aggregates/slt_good_10.test` | pass | 27.76 |
| postgres-extended | `random/expr/slt_good_0.test` | expected divergence | 13.25 |
| postgres-extended | `random/expr/slt_good_1.test` | pass | 9.87 |
| postgres-extended | `random/expr/slt_good_10.test` | pass | 16.09 |
| postgres-extended | `random/groupby/slt_good_0.test` | pass | 24.91 |
| postgres-extended | `random/groupby/slt_good_1.test` | pass | 25.18 |
| postgres-extended | `random/select/slt_good_0.test` | expected divergence | 19.67 |
| postgres-extended | `random/select/slt_good_1.test` | pass | 29.02 |
| postgres-extended | `select1.test` | pass | 23.94 |
| postgres-extended | `select2.test` | pass | 15.13 |
| postgres-extended | `select3.test` | pass | 54.95 |

## Expected divergences

- `postgres:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
- `postgres-extended:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres-extended:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres-extended:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres-extended:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
