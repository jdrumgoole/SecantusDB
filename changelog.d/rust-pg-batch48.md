### Rust PostgreSQL server: precise row waits, plan reuse, streaming portals, PG 15 catalog columns

#### Fixed

- A transaction waiting on a row now waits only on that row's holder. The holder is tracked through each block's written rows, including rows written by triggers and FK cascades. A chain of waits no longer reports a false deadlock (40P01); real cycles still do.
- `SELECT *` over 51 system catalogs returns exactly PostgreSQL 15's columns, in PostgreSQL 15's order. 21 differed before.
- An int8 value too large for an int4 column answers 22003, not 22P02.

#### Performance

- A prepared SELECT / UPDATE / DELETE that is safe to template reuses its plan across Executes, substituting the bound values into its WHERE filter.
- An extended-protocol SELECT outside a transaction block streams from one snapshot in 256-row batches on a pooled reader thread. Server memory for a 400 MB result grew 436 MB instead of 1,620 MB.

| µs per statement (release) | before | after | PostgreSQL 15 |
| --- | --- | --- | --- |
| extended primary-key read | 74 | 55 | 34 |
| simple `select 1` | 37 | 32 | 23 |
| extended scan, 1,000 rows | 891 | 755 | 188 |
