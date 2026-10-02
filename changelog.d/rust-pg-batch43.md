### Rust PostgreSQL server: privilege enforcement, and the per-statement cost halved

#### Fixed

- Privileges that were recorded but never enforced now are:
  - A schema without USAGE is left out of the search path.
  - Schema CREATE is checked when an object is created.
  - `nextval` / `currval` / `setval` and `SELECT` from a sequence check its privileges.
  - Function EXECUTE is checked at the call.
- Function grants are per signature. `GRANT ... ON ALL SEQUENCES / FUNCTIONS IN SCHEMA` takes effect.
- A dropped sequence's or function's grants no longer carry over to a recreated object of the same name.
- A PL/pgSQL or DO-block `RAISE NOTICE` is sent as it is raised.

#### Performance

Per-statement overhead had tripled since the full PostgreSQL 15 setting list was added. Release build, psycopg, median of 5 × 1,000:

| µs per statement | before | after |
| --- | --- | --- |
| simple `select 1` | 78 | 37 |
| extended `select 1` | 152 | 60 |
| extended primary-key read | 205 | 103 |

The setting map is reinstalled only when it changes. Roles, row-level-security flags, inheritance and expression-index lists are cached against the catalog version.
