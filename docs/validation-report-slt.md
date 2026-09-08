# sqllogictest conformance report

SecantusDB (Python server) 0.6.0b16 · corpus `gregrahn/sqllogictest` @ `c67f97bf3ca7` · sqllogictest-rs over pgwire · 2026-09-07

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
| postgres | `index/orderby/10/slt_good_0.test` | pass | 52.04 |
| postgres | `index/between/1/slt_good_0.test` | pass | 132.57 |
| postgres | `index/commute/10/slt_good_0.test` | pass | 49.27 |
| postgres | `index/delete/1/slt_good_0.test` | pass | 28.88 |
| postgres | `index/in/10/slt_good_0.test` | pass | 135.99 |
| postgres | `random/aggregates/slt_good_0.test` | expected divergence | 5.9 |
| postgres | `random/aggregates/slt_good_1.test` | pass | 22.53 |
| postgres | `random/aggregates/slt_good_10.test` | pass | 23.37 |
| postgres | `random/expr/slt_good_0.test` | expected divergence | 11.57 |
| postgres | `random/expr/slt_good_1.test` | pass | 8.41 |
| postgres | `random/expr/slt_good_10.test` | pass | 14.5 |
| postgres | `random/groupby/slt_good_0.test` | pass | 20.91 |
| postgres | `random/groupby/slt_good_1.test` | pass | 20.83 |
| postgres | `random/select/slt_good_0.test` | expected divergence | 16.55 |
| postgres | `random/select/slt_good_1.test` | pass | 24.45 |
| postgres | `select1.test` | pass | 28.9 |
| postgres | `select2.test` | pass | 16.86 |
| postgres | `select3.test` | pass | 62.55 |
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
| postgres-extended | `index/orderby/10/slt_good_0.test` | pass | 83.77 |
| postgres-extended | `index/between/1/slt_good_0.test` | pass | 171.95 |
| postgres-extended | `index/commute/10/slt_good_0.test` | pass | 73.13 |
| postgres-extended | `index/delete/1/slt_good_0.test` | pass | 44.2 |
| postgres-extended | `index/in/10/slt_good_0.test` | pass | 172.97 |
| postgres-extended | `random/aggregates/slt_good_0.test` | expected divergence | 10.17 |
| postgres-extended | `random/aggregates/slt_good_1.test` | pass | 38.37 |
| postgres-extended | `random/aggregates/slt_good_10.test` | pass | 39.68 |
| postgres-extended | `random/expr/slt_good_0.test` | expected divergence | 18.41 |
| postgres-extended | `random/expr/slt_good_1.test` | pass | 14.35 |
| postgres-extended | `random/expr/slt_good_10.test` | pass | 22.84 |
| postgres-extended | `random/groupby/slt_good_0.test` | pass | 35.44 |
| postgres-extended | `random/groupby/slt_good_1.test` | pass | 35.11 |
| postgres-extended | `random/select/slt_good_0.test` | expected divergence | 28.04 |
| postgres-extended | `random/select/slt_good_1.test` | pass | 41.1 |
| postgres-extended | `select1.test` | pass | 32.06 |
| postgres-extended | `select2.test` | pass | 19.83 |
| postgres-extended | `select3.test` | pass | 72.14 |

## Expected divergences

- `postgres:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
- `postgres-extended:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres-extended:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres-extended:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres-extended:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
