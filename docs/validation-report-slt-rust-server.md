# sqllogictest conformance report

SecantusDB (Rust PostgreSQL server, `secantusd-pg`) 0.1.0-beta.17 · corpus `gregrahn/sqllogictest` @ `c67f97bf3ca7` · sqllogictest-rs over pgwire · 2026-11-16

**42/60 files pass end-to-end** (18 expected divergences, 0 unexpected failures).

Regenerate with `uv run python -m invoke validate-slt --server rust`.

| lane | file | result | seconds |
|---|---|---|---:|
| postgres | `evidence/in1.test` | pass | 0.0 |
| postgres | `evidence/in2.test` | expected divergence | 0.03 |
| postgres | `evidence/slt_lang_aggfunc.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_createtrigger.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_createview.test` | expected divergence | 0.83 |
| postgres | `evidence/slt_lang_dropindex.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_droptable.test` | pass | 0.03 |
| postgres | `evidence/slt_lang_droptrigger.test` | pass | 0.02 |
| postgres | `evidence/slt_lang_dropview.test` | pass | 0.91 |
| postgres | `evidence/slt_lang_reindex.test` | pass | 0.11 |
| postgres | `evidence/slt_lang_replace.test` | pass | 0.0 |
| postgres | `evidence/slt_lang_update.test` | expected divergence | 0.04 |
| postgres | `index/orderby/10/slt_good_0.test` | pass | 19.92 |
| postgres | `index/between/1/slt_good_0.test` | pass | 46.91 |
| postgres | `index/commute/10/slt_good_0.test` | pass | 22.35 |
| postgres | `index/delete/1/slt_good_0.test` | pass | 35.1 |
| postgres | `index/in/10/slt_good_0.test` | pass | 47.89 |
| postgres | `random/aggregates/slt_good_0.test` | expected divergence | 3.64 |
| postgres | `random/aggregates/slt_good_1.test` | pass | 13.95 |
| postgres | `random/aggregates/slt_good_10.test` | pass | 14.61 |
| postgres | `random/expr/slt_good_0.test` | expected divergence | 6.76 |
| postgres | `random/expr/slt_good_1.test` | pass | 8.5 |
| postgres | `random/expr/slt_good_10.test` | expected divergence | 0.91 |
| postgres | `random/groupby/slt_good_0.test` | expected divergence | 6.67 |
| postgres | `random/groupby/slt_good_1.test` | expected divergence | 0.15 |
| postgres | `random/select/slt_good_0.test` | expected divergence | 9.23 |
| postgres | `random/select/slt_good_1.test` | pass | 13.71 |
| postgres | `select1.test` | pass | 12.12 |
| postgres | `select2.test` | pass | 8.19 |
| postgres | `select3.test` | pass | 32.08 |
| postgres-extended | `evidence/in1.test` | pass | 0.0 |
| postgres-extended | `evidence/in2.test` | expected divergence | 0.03 |
| postgres-extended | `evidence/slt_lang_aggfunc.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_createtrigger.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_createview.test` | expected divergence | 0.85 |
| postgres-extended | `evidence/slt_lang_dropindex.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_droptable.test` | pass | 0.03 |
| postgres-extended | `evidence/slt_lang_droptrigger.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_dropview.test` | pass | 0.93 |
| postgres-extended | `evidence/slt_lang_reindex.test` | pass | 0.02 |
| postgres-extended | `evidence/slt_lang_replace.test` | pass | 0.0 |
| postgres-extended | `evidence/slt_lang_update.test` | expected divergence | 0.07 |
| postgres-extended | `index/orderby/10/slt_good_0.test` | pass | 40.7 |
| postgres-extended | `index/between/1/slt_good_0.test` | pass | 112.22 |
| postgres-extended | `index/commute/10/slt_good_0.test` | pass | 46.3 |
| postgres-extended | `index/delete/1/slt_good_0.test` | pass | 47.97 |
| postgres-extended | `index/in/10/slt_good_0.test` | pass | 113.15 |
| postgres-extended | `random/aggregates/slt_good_0.test` | expected divergence | 7.27 |
| postgres-extended | `random/aggregates/slt_good_1.test` | pass | 27.73 |
| postgres-extended | `random/aggregates/slt_good_10.test` | pass | 28.99 |
| postgres-extended | `random/expr/slt_good_0.test` | expected divergence | 15.08 |
| postgres-extended | `random/expr/slt_good_1.test` | pass | 18.45 |
| postgres-extended | `random/expr/slt_good_10.test` | expected divergence | 1.7 |
| postgres-extended | `random/groupby/slt_good_0.test` | expected divergence | 14.02 |
| postgres-extended | `random/groupby/slt_good_1.test` | expected divergence | 0.27 |
| postgres-extended | `random/select/slt_good_0.test` | expected divergence | 17.84 |
| postgres-extended | `random/select/slt_good_1.test` | pass | 26.48 |
| postgres-extended | `select1.test` | pass | 20.8 |
| postgres-extended | `select2.test` | pass | 14.5 |
| postgres-extended | `select3.test` | pass | 57.1 |

## Expected divergences

- `postgres:evidence/in2.test` — corpus expects SQLite's empty `IN ()`; PostgreSQL rejects it as a syntax error (42601) at line 81
- `postgres:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres:evidence/slt_lang_update.test` — corpus expects SQLite to accept `SET x=3, x=4, x=5`; PostgreSQL raises 42601 'multiple assignments to same column' at line 88
- `postgres:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres:random/expr/slt_good_0.test` — corpus expects SQLite's integer result 0 for NULLIF(int, numeric) / int; PostgreSQL 15.19 computes in numeric (58.2...) and stops at line 45345. The Rust server gives 0 there (RP-076) and stops later, at 62223
- `postgres:random/expr/slt_good_10.test` — corpus expects SQLite's integer arithmetic (-1) where AVG makes the expression numeric on PostgreSQL (-1.1886...), at line 4792
- `postgres:random/groupby/slt_good_0.test` — corpus expects SQLite to accept an ungrouped column inside COALESCE; PostgreSQL raises 42803 at line 30126
- `postgres:random/groupby/slt_good_1.test` — corpus expects SQLite to accept an ungrouped column inside COALESCE; PostgreSQL raises 42803 at line 476
- `postgres:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
- `postgres-extended:evidence/in2.test` — corpus expects SQLite's empty `IN ()`; PostgreSQL rejects it as a syntax error (42601) at line 81
- `postgres-extended:evidence/slt_lang_createview.test` — corpus expects SQLite read-only views; real Postgres auto-updates simple views (DELETE/UPDATE/INSERT on view1 succeed here, as on PG)
- `postgres-extended:evidence/slt_lang_update.test` — corpus expects SQLite to accept `SET x=3, x=4, x=5`; PostgreSQL raises 42601 'multiple assignments to same column' at line 88
- `postgres-extended:random/aggregates/slt_good_0.test` — corpus expects SQLite's division-by-zero -> NULL; PG (and we) raise SQLSTATE 22012 (~22k records in)
- `postgres-extended:random/expr/slt_good_0.test` — corpus expects SQLite's integer result 0 for NULLIF(int, numeric) / int; PostgreSQL 15.19 computes in numeric (58.2...) and stops at line 45345. The Rust server gives 0 there (RP-076) and stops later, at 62223
- `postgres-extended:random/expr/slt_good_10.test` — corpus expects SQLite's integer arithmetic (-1) where AVG makes the expression numeric on PostgreSQL (-1.1886...), at line 4792
- `postgres-extended:random/groupby/slt_good_0.test` — corpus expects SQLite to accept an ungrouped column inside COALESCE; PostgreSQL raises 42803 at line 30126
- `postgres-extended:random/groupby/slt_good_1.test` — corpus expects SQLite to accept an ungrouped column inside COALESCE; PostgreSQL raises 42803 at line 476
- `postgres-extended:random/select/slt_good_0.test` — the corpus expects the RUNNER to cast REAL results to int per the 'query I' type string; sqllogictest-rs doesn't (~52k records in)
