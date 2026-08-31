# sqllogictest conformance report

SecantusDB (Python server) 0.6.0b16 · corpus `gregrahn/sqllogictest` @ `c67f97bf3ca7` · sqllogictest-rs over pgwire · 2026-08-31

**52/60 files pass end-to-end** (8 expected divergences, 0 unexpected failures).

Regenerate with `uv run python -m invoke validate-slt`.

| lane | file | result | seconds |
|---|---|---|---:|
| postgres | `evidence/in1.test` | pass | 0.0 |
| postgres | `evidence/in2.test` | pass | 0.09 |
| postgres | `evidence/slt_lang_aggfunc.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_createtrigger.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_createview.test` | expected divergence | 0.05 |
| postgres | `evidence/slt_lang_dropindex.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_droptable.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_droptrigger.test` | pass | 0.01 |
| postgres | `evidence/slt_lang_dropview.test` | pass | 0.03 |
| postgres | `evidence/slt_lang_reindex.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_replace.test` | pass | 0.0 |
| postgres | `evidence/slt_lang_update.test` | pass | 0.06 |
| postgres | `index/orderby/10/slt_good_0.test` | pass | 42.62 |
| postgres | `index/between/1/slt_good_0.test` | pass | 104.26 |
| postgres | `index/commute/10/slt_good_0.test` | pass | 40.58 |
| postgres | `index/delete/1/slt_good_0.test` | pass | 24.81 |
| postgres | `index/in/10/slt_good_0.test` | pass | 104.27 |
| postgres | `random/aggregates/slt_good_0.test` | expected divergence | 5.04 |
| postgres | `random/aggregates/slt_good_1.test` | pass | 19.32 |
| postgres | `random/aggregates/slt_good_10.test` | pass | 20.02 |
| postgres | `random/expr/slt_good_0.test` | expected divergence | 9.17 |
| postgres | `random/expr/slt_good_1.test` | pass | 7.03 |
| postgres | `random/expr/slt_good_10.test` | pass | 11.29 |
| postgres | `random/groupby/slt_good_0.test` | pass | 18.54 |
| postgres | `random/groupby/slt_good_1.test` | pass | 18.57 |
| postgres | `random/select/slt_good_0.test` | expected divergence | 14.6 |
| postgres | `random/select/slt_good_1.test` | pass | 21.3 |
| postgres | `select1.test` | pass | 24.5 |
| postgres | `select2.test` | pass | 14.26 |
| postgres | `select3.test` | pass | 52.25 |
| postgres-extended | `evidence/in1.test` | pass | 0.0 |
| postgres-extended | `evidence/in2.test` | pass | 0.14 |
| postgres-extended | `evidence/slt_lang_aggfunc.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_createtrigger.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_createview.test` | expected divergence | 0.03 |
| postgres-extended | `evidence/slt_lang_dropindex.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_droptable.test` | pass | 0.03 |
| postgres-extended | `evidence/slt_lang_droptrigger.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_dropview.test` | pass | 0.04 |
| postgres-extended | `evidence/slt_lang_reindex.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_replace.test` | pass | 0.0 |
| postgres-extended | `evidence/slt_lang_update.test` | pass | 0.07 |
| postgres-extended | `index/orderby/10/slt_good_0.test` | pass | 68.45 |
| postgres-extended | `index/between/1/slt_good_0.test` | pass | 129.68 |
| postgres-extended | `index/commute/10/slt_good_0.test` | pass | 61.05 |
| postgres-extended | `index/delete/1/slt_good_0.test` | pass | 37.95 |
| postgres-extended | `index/in/10/slt_good_0.test` | pass | 132.33 |
| postgres-extended | `random/aggregates/slt_good_0.test` | expected divergence | 8.84 |
| postgres-extended | `random/aggregates/slt_good_1.test` | pass | 33.52 |
| postgres-extended | `random/aggregates/slt_good_10.test` | pass | 34.75 |
| postgres-extended | `random/expr/slt_good_0.test` | expected divergence | 14.47 |
| postgres-extended | `random/expr/slt_good_1.test` | pass | 12.42 |
| postgres-extended | `random/expr/slt_good_10.test` | pass | 17.65 |
| postgres-extended | `random/groupby/slt_good_0.test` | pass | 31.49 |
| postgres-extended | `random/groupby/slt_good_1.test` | pass | 31.53 |
| postgres-extended | `random/select/slt_good_0.test` | expected divergence | 24.73 |
| postgres-extended | `random/select/slt_good_1.test` | pass | 36.36 |
| postgres-extended | `select1.test` | pass | 26.76 |
| postgres-extended | `select2.test` | pass | 16.53 |
| postgres-extended | `select3.test` | pass | 61.74 |

## Expected divergences

- `postgres:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
- `postgres-extended:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres-extended:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres-extended:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres-extended:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
