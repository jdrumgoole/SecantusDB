### Python PostgreSQL server: trigger arguments, UPDATE OF and TRUNCATE triggers

The Python PG server now runs three trigger shapes it used to refuse, each checked against PostgreSQL 15's output.

#### Added

- Trigger arguments: `EXECUTE FUNCTION f('x', 7)` is stored as text and exposed to PL/pgSQL as `TG_ARGV` (subscripted from 0) and `TG_NARGS`.
- `UPDATE OF col, ...` triggers. One fires when any listed column is a target of the `SET` list, whether or not its value changes.
- `BEFORE` / `AFTER TRUNCATE` statement triggers. A truncate that fires one runs as a single transaction, so a trigger that raises leaves the rows in place.
