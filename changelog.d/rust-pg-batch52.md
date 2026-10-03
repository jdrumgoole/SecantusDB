### Rust PostgreSQL server: faster correlated subqueries, `sum(bigint)` overflow, `pg_collation_for`

#### Fixed

- `sum(bigint)` past the int64 range silently wrapped (`-9223372036854775806` where PostgreSQL answers `9223372036854775810`). It now sums exactly and answers numeric.
- `pg_collation_for`:
  - raises 42804 over a non-collatable type;
  - answers NULL for an untyped literal;
  - keeps its column name.
- A COPY or Describe that arrived before any statement had run on its worker thread could miss user-defined types.

#### Performance

Correlated subqueries, 2,000 × 2,000 rows, release build:

| shape | before | after | PostgreSQL 15 |
| --- | --- | --- | --- |
| nested correlated EXISTS | 36.7 s | 1.21 s | 0.13 s |
| `sum` / `avg` under a filter | 1.2 s | 0.03-0.04 s | 0.15 s |
| inner `ORDER BY ... LIMIT 1` | 0.30 s | 0.008 s | 0.13 s |

- A WHERE with one non-lowerable conjunct now lowers the rest instead of evaluating the whole clause per row.
- BindComplete and CloseComplete wait for Sync or Flush. A primary-key read went from 54 to 50 µs.
