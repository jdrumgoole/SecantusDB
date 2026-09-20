### The Rust PostgreSQL server answers UPDATE and DELETE RETURNING

`update t set ... returning id, n` and `delete from t ... returning *` did the
write, reported the right tag, and returned **no rowset at all** — a client
that asked which rows it had just changed got nothing back. The clause was
parsed and dropped: neither plan carried it. UPDATE now returns each row as it
is after the update and DELETE as it was before, which is what PostgreSQL does.

Found by sweeping the existing `pg_corpora/` corpora against the Rust server,
which the differential probe can now drive.

#### Fixed

- `crates/secantus-pgplan`: `Update` and `Delete` carry a `returning` list.
- `crates/secantus-pgserver`: both project their affected rows.
- `crates/secantus-pgserver`: an aliased PRIMARY KEY kept its type. A key is
  stored as `_id`, and the row description looked the column up by its stored
  field and then by its output name, so `select id as k` matched neither and
  fell back to `varchar` — the client decoded an integer as a string. Only
  aliased primary keys were affected.

#### Added

- `tools/probes/pg_differential.py --server=rust` runs any existing corpus
  against the Rust server instead of the Python one.

#### Testing

- `tests/test_rust_pgserver_slice.py`: UPDATE / DELETE RETURNING including a
  no-match statement, and an aliased key's type through both SELECT and
  RETURNING.
