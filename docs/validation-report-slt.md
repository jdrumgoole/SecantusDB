# sqllogictest conformance report

SecantusDB (Python server) 0.7.0b3 · corpus `gregrahn/sqllogictest` @ `c67f97bf3ca7` · sqllogictest-rs over pgwire · 2026-10-19

**52/60 files pass end-to-end** (8 expected divergences, 0 unexpected failures).

Regenerate with `uv run python -m invoke validate-slt`.

| lane | file | result | seconds |
|---|---|---|---:|
| postgres | `evidence/in1.test` | pass | 0.0 |
| postgres | `evidence/in2.test` | pass | 0.1 |
| postgres | `evidence/slt_lang_aggfunc.test` | pass | 0.04 |
| postgres | `evidence/slt_lang_createtrigger.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_createview.test` | expected divergence | 0.02 |
| postgres | `evidence/slt_lang_dropindex.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_droptable.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_droptrigger.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_dropview.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_reindex.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_replace.test` | pass | 0.0 |
| postgres | `evidence/slt_lang_update.test` | pass | 0.06 |
| postgres | `index/orderby/10/slt_good_0.test` | pass | 44.89 |
| postgres | `index/between/1/slt_good_0.test` | pass | 116.52 |
| postgres | `index/commute/10/slt_good_0.test` | pass | 40.05 |
| postgres | `index/delete/1/slt_good_0.test` | pass | 26.8 |
| postgres | `index/in/10/slt_good_0.test` | pass | 112.52 |
| postgres | `random/aggregates/slt_good_0.test` | expected divergence | 4.66 |
| postgres | `random/aggregates/slt_good_1.test` | pass | 18.12 |
| postgres | `random/aggregates/slt_good_10.test` | pass | 18.12 |
| postgres | `random/expr/slt_good_0.test` | expected divergence | 8.82 |
| postgres | `random/expr/slt_good_1.test` | pass | 6.1 |
| postgres | `random/expr/slt_good_10.test` | pass | 11.21 |
| postgres | `random/groupby/slt_good_0.test` | pass | 16.84 |
| postgres | `random/groupby/slt_good_1.test` | pass | 16.87 |
| postgres | `random/select/slt_good_0.test` | expected divergence | 13.11 |
| postgres | `random/select/slt_good_1.test` | pass | 19.74 |
| postgres | `select1.test` | pass | 23.17 |
| postgres | `select2.test` | pass | 13.72 |
| postgres | `select3.test` | pass | 49.13 |
| postgres-extended | `evidence/in1.test` | pass | 0.0 |
| postgres-extended | `evidence/in2.test` | pass | 0.14 |
| postgres-extended | `evidence/slt_lang_aggfunc.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_createtrigger.test` | pass | 0.01 |
| postgres-extended | `evidence/slt_lang_createview.test` | expected divergence | 0.03 |
| postgres-extended | `evidence/slt_lang_dropindex.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_droptable.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_droptrigger.test` | pass | 0.19 |
| postgres-extended | `evidence/slt_lang_dropview.test` | pass | 0.03 |
| postgres-extended | `evidence/slt_lang_reindex.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_replace.test` | pass | 0.0 |
| postgres-extended | `evidence/slt_lang_update.test` | pass | 0.07 |
| postgres-extended | `index/orderby/10/slt_good_0.test` | pass | 69.8 |
| postgres-extended | `index/between/1/slt_good_0.test` | pass | 148.7 |
| postgres-extended | `index/commute/10/slt_good_0.test` | pass | 62.35 |
| postgres-extended | `index/delete/1/slt_good_0.test` | pass | 40.21 |
| postgres-extended | `index/in/10/slt_good_0.test` | pass | 144.49 |
| postgres-extended | `random/aggregates/slt_good_0.test` | expected divergence | 8.1 |
| postgres-extended | `random/aggregates/slt_good_1.test` | pass | 31.02 |
| postgres-extended | `random/aggregates/slt_good_10.test` | pass | 31.74 |
| postgres-extended | `random/expr/slt_good_0.test` | expected divergence | 13.74 |
| postgres-extended | `random/expr/slt_good_1.test` | pass | 10.02 |
| postgres-extended | `random/expr/slt_good_10.test` | pass | 16.89 |
| postgres-extended | `random/groupby/slt_good_0.test` | pass | 28.52 |
| postgres-extended | `random/groupby/slt_good_1.test` | pass | 28.46 |
| postgres-extended | `random/select/slt_good_0.test` | expected divergence | 22.24 |
| postgres-extended | `random/select/slt_good_1.test` | pass | 32.33 |
| postgres-extended | `select1.test` | pass | 26.08 |
| postgres-extended | `select2.test` | pass | 16.04 |
| postgres-extended | `select3.test` | pass | 58.11 |

## Expected divergences

- `postgres:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
- `postgres-extended:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres-extended:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres-extended:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres-extended:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
