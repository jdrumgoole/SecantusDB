# sqllogictest conformance report

SecantusDB (Python server) 0.6.0b16 · corpus `gregrahn/sqllogictest` @ `c67f97bf3ca7` · sqllogictest-rs over pgwire · 2026-09-21

**52/60 files pass end-to-end** (8 expected divergences, 0 unexpected failures).

Regenerate with `uv run python -m invoke validate-slt`.

| lane | file | result | seconds |
|---|---|---|---:|
| postgres | `evidence/in1.test` | pass | 0.0 |
| postgres | `evidence/in2.test` | pass | 0.16 |
| postgres | `evidence/slt_lang_aggfunc.test` | pass | 0.16 |
| postgres | `evidence/slt_lang_createtrigger.test` | pass | 0.14 |
| postgres | `evidence/slt_lang_createview.test` | expected divergence | 0.03 |
| postgres | `evidence/slt_lang_dropindex.test` | pass | 0.07 |
| postgres | `evidence/slt_lang_droptable.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_droptrigger.test` | pass | 0.11 |
| postgres | `evidence/slt_lang_dropview.test` | pass | 0.03 |
| postgres | `evidence/slt_lang_reindex.test` | pass | 0.35 |
| postgres | `evidence/slt_lang_replace.test` | pass | 0.0 |
| postgres | `evidence/slt_lang_update.test` | pass | 0.41 |
| postgres | `index/orderby/10/slt_good_0.test` | pass | 47.0 |
| postgres | `index/between/1/slt_good_0.test` | pass | 115.37 |
| postgres | `index/commute/10/slt_good_0.test` | pass | 44.67 |
| postgres | `index/delete/1/slt_good_0.test` | pass | 26.84 |
| postgres | `index/in/10/slt_good_0.test` | pass | 117.51 |
| postgres | `random/aggregates/slt_good_0.test` | expected divergence | 5.6 |
| postgres | `random/aggregates/slt_good_1.test` | pass | 20.92 |
| postgres | `random/aggregates/slt_good_10.test` | pass | 22.18 |
| postgres | `random/expr/slt_good_0.test` | expected divergence | 10.51 |
| postgres | `random/expr/slt_good_1.test` | pass | 7.4 |
| postgres | `random/expr/slt_good_10.test` | pass | 13.08 |
| postgres | `random/groupby/slt_good_0.test` | pass | 19.99 |
| postgres | `random/groupby/slt_good_1.test` | pass | 19.53 |
| postgres | `random/select/slt_good_0.test` | expected divergence | 15.3 |
| postgres | `random/select/slt_good_1.test` | pass | 21.92 |
| postgres | `select1.test` | pass | 23.84 |
| postgres | `select2.test` | pass | 13.79 |
| postgres | `select3.test` | pass | 51.72 |
| postgres-extended | `evidence/in1.test` | pass | 0.0 |
| postgres-extended | `evidence/in2.test` | pass | 0.18 |
| postgres-extended | `evidence/slt_lang_aggfunc.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_createtrigger.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_createview.test` | expected divergence | 0.13 |
| postgres-extended | `evidence/slt_lang_dropindex.test` | pass | 0.04 |
| postgres-extended | `evidence/slt_lang_droptable.test` | pass | 0.03 |
| postgres-extended | `evidence/slt_lang_droptrigger.test` | pass | 0.08 |
| postgres-extended | `evidence/slt_lang_dropview.test` | pass | 0.06 |
| postgres-extended | `evidence/slt_lang_reindex.test` | pass | 0.03 |
| postgres-extended | `evidence/slt_lang_replace.test` | pass | 0.0 |
| postgres-extended | `evidence/slt_lang_update.test` | pass | 0.09 |
| postgres-extended | `index/orderby/10/slt_good_0.test` | pass | 78.65 |
| postgres-extended | `index/between/1/slt_good_0.test` | pass | 152.15 |
| postgres-extended | `index/commute/10/slt_good_0.test` | pass | 68.72 |
| postgres-extended | `index/delete/1/slt_good_0.test` | pass | 42.05 |
| postgres-extended | `index/in/10/slt_good_0.test` | pass | 155.44 |
| postgres-extended | `random/aggregates/slt_good_0.test` | expected divergence | 9.47 |
| postgres-extended | `random/aggregates/slt_good_1.test` | pass | 36.0 |
| postgres-extended | `random/aggregates/slt_good_10.test` | pass | 35.79 |
| postgres-extended | `random/expr/slt_good_0.test` | expected divergence | 15.17 |
| postgres-extended | `random/expr/slt_good_1.test` | pass | 11.51 |
| postgres-extended | `random/expr/slt_good_10.test` | pass | 19.66 |
| postgres-extended | `random/groupby/slt_good_0.test` | pass | 32.86 |
| postgres-extended | `random/groupby/slt_good_1.test` | pass | 33.34 |
| postgres-extended | `random/select/slt_good_0.test` | expected divergence | 25.65 |
| postgres-extended | `random/select/slt_good_1.test` | pass | 36.88 |
| postgres-extended | `select1.test` | pass | 26.85 |
| postgres-extended | `select2.test` | pass | 17.58 |
| postgres-extended | `select3.test` | pass | 62.28 |

## Expected divergences

- `postgres:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
- `postgres-extended:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres-extended:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres-extended:random/expr/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~75k records in)
- `postgres-extended:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
