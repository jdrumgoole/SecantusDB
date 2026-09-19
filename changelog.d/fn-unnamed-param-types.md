### `CREATE FUNCTION f(int, int)` recorded its arguments as `void`

`pg_proc.proargtypes` read `'2278 2278'` for a function declared with **unnamed**
parameters — 2278 is `void`, and an OID this catalog does not even define, so a
client resolving it found nothing. Named parameters were unaffected:
`CREATE FUNCTION g(a int, b text)` recorded `'23 25'` correctly all along, which
is why a catalog full of working functions never showed it.

#### Fixed

- `_function_param_types` now resolves a bare parameter type. sqlglot parses
  `f(a int)` as a `ColumnDef` carrying a `DataType`, but bare `f(int, int)` as a
  plain `Identifier` whose name is the type spelling; the `DataType` branch
  missed it and the tag fell through to `None` → void. Multi-word builtins
  resolve too (`double precision` → `float8`).

Found from pgjdbc's `DatabaseMetaDataTest::funcWithoutNames()`, which is exactly
the test for functions declared without parameter names. Measured effect on that
gauge: 82 → 80 standing failures, and the baseline is tightened to match.
