### The Rust PostgreSQL server verifies passwords, and gains LATERAL and EXPLAIN

A role created with a password now has to prove it. The Rust PostgreSQL server
stored the SCRAM-SHA-256 verifier `CREATE ROLE ... PASSWORD` gives it, and then
trusted every connection anyway -- a wrong password, or none, logged in as the
role. It now runs the SCRAM-SHA-256 exchange against the stored verifier, and
refuses a NOLOGIN role. A role with no password, and a user the server has
never heard of, are still trusted, which is what test fixtures connecting as a
password-less `postgres` rely on.

`LATERAL` works, for subqueries and for functions (which are implicitly
lateral, as in PostgreSQL), in comma, CROSS and LEFT joins. A set-returning
function over a column in the select list -- `SELECT unnest(tags) FROM t` --
works, planned as the lateral join it means. `EXPLAIN` answers with the plan's
shape in PostgreSQL's layout; it prints zero costs rather than invented ones.

#### Added

- SCRAM-SHA-256 password verification (`secantus-auth` gains the PostgreSQL
  form of the exchange: an empty client user name and the `y,,` header).
- `LATERAL` subqueries and functions; select-list set-returning functions;
  `jsonb_array_elements[_text]`, `jsonb_each[_text]`, `jsonb_object_keys` and
  their `json_` twins; `regexp_matches`; multi-array `unnest(a, b)`.
- `EXPLAIN [ANALYZE]`, `(COSTS OFF)`, `(FORMAT JSON)`.
- `COLLATE "C"` (and `POSIX` / `default` / `ucs_basic`).

#### Fixed

- `regexp_count` / `_instr` / `_substr` / `_like` and `to_ascii` answer as
  PostgreSQL 14 does (42883, 0A000) rather than "not supported yet".
- `unnest(a) AS x` names its column `x`, as PostgreSQL's rule for a function
  returning one column has it.

#### Added (foreign keys)

- Multi-column FOREIGN KEYs, to a composite PRIMARY KEY or to a UNIQUE
  constraint; `ON DELETE` / `ON UPDATE` with `CASCADE`, `SET NULL` and the new
  `SET DEFAULT`. MATCH SIMPLE: a key with a NULL column references nothing.
