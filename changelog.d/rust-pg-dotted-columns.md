### Rust PostgreSQL server: column names with a dot or a leading `$`

A column named like `"dot.s"` or `"$x"` was read as a nested MongoDB-style path. `WHERE` matched nothing, `UPDATE` / `DELETE` reported rows but changed nothing, and `UNIQUE` was never enforced. These columns now behave as in PostgreSQL 15, pinned by the new `dotted_columns` corpus. The SQLAlchemy dialect suite against the Rust server is now 978 passed / 0 failed.

#### Fixed

- `WHERE` over such a column is evaluated against the literal key.
- `UPDATE` and `ON CONFLICT DO UPDATE` write the literal key.
- `UNIQUE` constraints and unique indexes over such columns are enforced. An automatic index name containing a dot no longer breaks its hidden index field.
