### The Rust PostgreSQL server: user aggregates and operators, table inheritance, hash partitioning, pg_trgm, and operators resolved by type

Several queries answered an empty result where PostgreSQL refuses them or
returns rows. Each was silent: no error, just a wrong answer.

- **Comparisons across type categories.** A text column compared with an
  integer (`WHERE s = 1`) answered an empty result, as did a date compared
  with a number or a boolean compared with an integer. PostgreSQL refuses
  each at plan time with `42883 operator does not exist: text = integer`.
  The Rust server now resolves operators by type as PostgreSQL does.
- **A date column against a timestamp.** `WHERE d < now()` matched nothing.
  The date is now promoted to midnight, in the session zone for a
  `timestamptz`.
- **A binary FETCH from some cursors.** Over `VALUES`, an aggregate or a
  constant select, the FETCH sent text bytes that a binary client decoded as
  garbage integers.

Errors now carry PostgreSQL's position, so a client prints the caret under the
token at fault.

#### Added

- `CREATE AGGREGATE`, `CREATE OPERATOR`, `CREATE STATISTICS`,
  `CREATE PUBLICATION`, `CREATE TABLESPACE` and `SECURITY LABEL`, each with
  its catalog (`pg_aggregate`, `pg_operator`, `pg_statistic_ext`,
  `pg_publication*`, `pg_tablespace`).
- Table inheritance (`INHERITS`):
  - a read of a parent reads its descendants;
  - `UPDATE` / `DELETE` / the recursing `ALTER`s reach them too;
  - `tableoid` works on every table.
- `PARTITION BY HASH`, routed by PostgreSQL's own lookup3 hash, so a row lands
  in the partition PostgreSQL would choose. Partition keys may be
  expressions, and a multi-column key compares in declared order.
- `pg_trgm`: `similarity`, `word_similarity`, the `%` family of operators and
  their thresholds, and trigram opclasses.
- `ALTER VIEW` (owner, rename, rename column, defaults, options,
  `security_invoker`). Views follow the renames of what they read. A view's
  `*` is frozen at creation. `pg_views` and `information_schema.views` are
  populated.
- Other additions:
  - table locks held by a block's statements, deadlock detection (`40P01`),
    and `pg_locks`;
  - `DROP TABLE ... CASCADE`;
  - `ADD COLUMN ... serial`;
  - ON CONFLICT arbiters on unique and partial indexes, and key-changing
    upserts;
  - `DISTINCT` on the extended aggregates;
  - `STRICT` functions;
  - function-style casts.
- RANGE window frames with interval offsets over dates and times, and with
  fractional offsets over numbers. ROWS and GROUPS offsets are read as bigint.
- A `Bind` asking for MIXED per-column result formats is honoured column by
  column. A format count that does not match the columns is `08P01`.

#### Fixed

- Error codes and messages now match PostgreSQL:
  - `abs(text)` and the other numeric built-ins given a string are `42883`,
    as are `upper(1)` and the other text built-ins given a number;
  - an unknown function is `42883` at plan time, even over an empty table;
  - negative frame offsets are `22013`;
  - in a FROM-less select, a WHERE naming an output alias is `42703`, and a
    WHERE naming no column is evaluated once;
  - a recursive CTE whose anchor is narrower than its UNION's type is
    `42804`.
- An untyped literal compared with a date column is read as a date.
- A parameterised recursive CTE (`SELECT $1::int UNION ALL SELECT n + 1 ...`)
  typed its column as text and failed with `42883`. A VALUES column holding
  NULL under a cast now takes the cast's type.
- Date and time handling:
  - time-only input forms and PostgreSQL 15's full default zone-abbreviation
    set;
  - `DateStyle` in text casts and arrays;
  - `to_date` / `to_timestamp` stop at the end of the input, read ISO weeks,
    and number negative years as PostgreSQL does;
  - wide-year and BC `timestamptz` render in the session zone;
  - numeric and POSIX `TimeZone` settings are accepted;
  - daylight saving is applied past 2037.
- Float NaN compares above every number. Stored ranges render in the session
  zone, and a range casts to a multirange.
- `information_schema.columns.column_default` prints expressions as
  PostgreSQL's ruleutils does: negative constants typed by the literal,
  implicit numeric casts shown, `NOT` / `AND` / `OR` kept rather than folded,
  and arrays as typed literals.
- MERGE fires each action's statement triggers once, and acts on identical
  keyless rows as separate rows. Row-level security on a table read through a
  view applies the view owner's policies.
