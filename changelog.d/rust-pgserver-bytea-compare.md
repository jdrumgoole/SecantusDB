### Rust pgserver: bytea comparison operators

`bytea` values could be stored, concatenated and sliced but not COMPARED — the
Rust PostgreSQL server answered `0A000` for `bytea = bytea` and its siblings,
which failed the psycopg tests that filter or order by a `bytea`. The
comparison operators (`=`, `<>`, `<`, `<=`, `>`, `>=`) now work, ordering by
unsigned byte value lexicographically exactly as PostgreSQL does (a prefix
sorts before the longer value).

#### Added
- `bytea` comparison operators, ordering by unsigned byte value.
