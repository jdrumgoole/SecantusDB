# sqllogictest conformance report

SecantusDB (Python server) 0.6.0b17 · corpus `gregrahn/sqllogictest` @ `c67f97bf3ca7` · sqllogictest-rs over pgwire · 2026-10-05

**52/60 files pass end-to-end** (8 expected divergences, 0 unexpected failures).

Regenerate with `uv run python -m invoke validate-slt`.

| lane | file | result | seconds |
|---|---|---|---:|
| postgres | `evidence/in1.test` | pass | 0.0 |
| postgres | `evidence/in2.test` | pass | 0.08 |
| postgres | `evidence/slt_lang_aggfunc.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_createtrigger.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_createview.test` | expected divergence | 0.02 |
| postgres | `evidence/slt_lang_dropindex.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_droptable.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_droptrigger.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_dropview.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_reindex.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_replace.test` | pass | 0.0 |
| postgres | `evidence/slt_lang_update.test` | pass | 0.06 |
| postgres | `index/orderby/10/slt_good_0.test` | pass | 39.2 |
| postgres | `index/between/1/slt_good_0.test` | pass | 96.5 |
| postgres | `index/commute/10/slt_good_0.test` | pass | 38.2 |
| postgres | `index/delete/1/slt_good_0.test` | pass | 26.29 |
| postgres | `index/in/10/slt_good_0.test` | pass | 97.26 |
| postgres | `random/aggregates/slt_good_0.test` | expected divergence | 4.83 |
| postgres | `random/aggregates/slt_good_1.test` | pass | 17.82 |
| postgres | `random/aggregates/slt_good_10.test` | pass | 18.41 |
| postgres | `random/expr/slt_good_0.test` | expected divergence | 8.69 |
| postgres | `random/expr/slt_good_1.test` | pass | 6.01 |
| postgres | `random/expr/slt_good_10.test` | pass | 10.38 |
| postgres | `random/groupby/slt_good_0.test` | pass | 16.66 |
| postgres | `random/groupby/slt_good_1.test` | pass | 16.55 |
| postgres | `random/select/slt_good_0.test` | expected divergence | 12.9 |
| postgres | `random/select/slt_good_1.test` | pass | 18.7 |
| postgres | `select1.test` | pass | 19.64 |
| postgres | `select2.test` | pass | 11.98 |
| postgres | `select3.test` | pass | 43.7 |
| postgres-extended | `evidence/in1.test` | pass | 0.0 |
| postgres-extended | `evidence/in2.test` | pass | 0.16 |
| postgres-extended | `evidence/slt_lang_aggfunc.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_createtrigger.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_createview.test` | expected divergence | 0.04 |
| postgres-extended | `evidence/slt_lang_dropindex.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_droptable.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_droptrigger.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_dropview.test` | pass | 0.03 |
| postgres-extended | `evidence/slt_lang_reindex.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_replace.test` | pass | 0.0 |
| postgres-extended | `evidence/slt_lang_update.test` | pass | 0.08 |
| postgres-extended | `index/orderby/10/slt_good_0.test` | pass | 65.71 |
| postgres-extended | `index/between/1/slt_good_0.test` | pass | 132.76 |
| postgres-extended | `index/commute/10/slt_good_0.test` | pass | 58.2 |
| postgres-extended | `index/delete/1/slt_good_0.test` | pass | 38.98 |
| postgres-extended | `index/in/10/slt_good_0.test` | pass | 129.92 |
| postgres-extended | `random/aggregates/slt_good_0.test` | expected divergence | 7.83 |
| postgres-extended | `random/aggregates/slt_good_1.test` | pass | 30.55 |
| postgres-extended | `random/aggregates/slt_good_10.test` | pass | 31.36 |
| postgres-extended | `random/expr/slt_good_0.test` | expected divergence | 12.88 |
| postgres-extended | `random/expr/slt_good_1.test` | pass | 9.69 |
| postgres-extended | `random/expr/slt_good_10.test` | pass | 15.66 |
| postgres-extended | `random/groupby/slt_good_0.test` | pass | 28.45 |
| postgres-extended | `random/groupby/slt_good_1.test` | pass | 27.52 |
| postgres-extended | `random/select/slt_good_0.test` | expected divergence | 21.9 |
| postgres-extended | `random/select/slt_good_1.test` | pass | 31.93 |
| postgres-extended | `select1.test` | pass | 22.57 |
| postgres-extended | `select2.test` | pass | 14.44 |
| postgres-extended | `select3.test` | pass | 51.39 |

## Expected divergences

- `postgres:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
- `postgres-extended:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres-extended:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres-extended:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres-extended:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
