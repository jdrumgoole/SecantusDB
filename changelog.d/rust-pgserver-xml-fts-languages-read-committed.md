### The Rust PostgreSQL server: partitioning, row-level security, WITH RECURSIVE, domains, materialized views, READ COMMITTED, the xml type, and every text-search language

Inside a transaction block, each statement now sees what other connections
committed before it, as PostgreSQL's default READ COMMITTED does. That covers
rows, and tables created since the block began. An `UPDATE` of a row another
connection changed since the block began no longer fails with an internal
`WriteConflict`. One limit comes from WiredTiger: once a block has written,
it keeps its snapshot, as REPEATABLE READ would. A write that still conflicts
now reports PostgreSQL's retryable `40001`, not `XX000`.

#### Added

- Declarative partitioning, `PARTITION BY RANGE | LIST`:
  - `CREATE TABLE ... PARTITION OF ... FOR VALUES FROM / TO / IN / DEFAULT`,
    including multi-column ranges, `MINVALUE` / `MAXVALUE`, `NULL` list
    members and sub-partitions.
  - A row written to the parent goes to its partition. A row that fits
    none is `23514 no partition of relation ... found for row`. A row
    written to a partition must satisfy the partition's bound.
  - An `UPDATE` that changes the key moves the row between partitions.
  - `ATTACH` / `DETACH PARTITION` move the rows. `DROP` and `TRUNCATE` of a
    partition remove its rows. `COPY` works in and out of partitions.
  - `ALTER TABLE ADD COLUMN` on the parent reaches every partition.
  - Overlapping, empty and mismatched bounds are refused with PostgreSQL's
    errors.
  - Reflected in the catalog: `relkind 'p'`, `relispartition`,
    `pg_get_expr(relpartbound)`, `pg_inherits`, `pg_partitioned_table`, and
    per-partition indexes.
  - The `tableoid` system column names the partition a row is in.
  - A duplicate key names the partition's own constraint.
- `CREATE INDEX ... USING gin | gist | brin | spgist`, with PostgreSQL's
  operator-class rules. A type with no default class is `42704`, and an
  explicit class such as `jsonb_path_ops` or `text_pattern_ops` shows in
  `pg_indexes`. The `btree_gin` and `btree_gist` extensions add the scalar
  classes. Dropping one refuses (`2BP01`) or cascades to the indexes that
  use it.
- `UPDATE` of a `PRIMARY KEY` column, single or composite. Uniqueness is
  checked row by row as PostgreSQL checks it, so `SET id = id + 1` over
  `(1, 2)` collides, and nothing is written when it does.
- Row-level security:
  - `ALTER TABLE ... ENABLE | DISABLE | FORCE | NO FORCE ROW LEVEL SECURITY`,
    reflected in `pg_class.relrowsecurity` / `relforcerowsecurity`.
  - `CREATE` / `ALTER` / `DROP POLICY`, listed in `pg_policies` with
    PostgreSQL's rendering of `USING` and `WITH CHECK`.
  - Policies are recorded, not yet enforced; see the backlog.
- `ALTER TYPE ... ADD VALUE [BEFORE | AFTER]` and `RENAME VALUE`. Enum
  values order by declared position in `ORDER BY`, comparisons, `BETWEEN`,
  `min` / `max` and `GREATEST` / `LEAST`.
- Generated columns (`GENERATED ALWAYS AS (...) STORED`), recomputed on
  INSERT, UPDATE and COPY. Assigning one directly is `428C9`. Reflected in
  `information_schema.columns` and `pg_attrdef`.
- `CREATE TABLE ... (LIKE t INCLUDING ...)`.
- The jsonb functions: `jsonb_set`, `jsonb_set_lax`, `jsonb_insert`,
  `jsonb_pretty`, `jsonb_typeof`, `jsonb_array_length`,
  `jsonb_extract_path[_text]`, `json[b]_to_record[set]` and
  `json[b]_populate_record`, and the `-`, `#-` and `||` operators.
- Trigonometric, hyperbolic and degree functions, `cbrt`, `gcd`, `lcm`,
  `factorial`, `width_bucket`, `setseed`, and the `@`, `|/` and `||/`
  operators.
- `sha224` / `sha256` / `sha384` / `sha512`, `string_to_table`, and
  `format()` widths (`%-10s`, `%*s`).
- Advisory locks: `pg_advisory_lock` and its whole family, session-level and
  transaction-level, shared and exclusive, blocking and `try_`.

- `WITH RECURSIVE`, iterated to a fixed point: `UNION` stops a cycle,
  `UNION ALL` keeps every row, and a self-reference without a non-recursive
  term is `42P19`.
- `CREATE DOMAIN` / `ALTER DOMAIN` / `DROP DOMAIN`. A domain's NOT NULL,
  CHECK constraints and DEFAULT apply on INSERT, UPDATE and casts. The wire
  carries the base type, as PostgreSQL's does, and domains are reflected in
  `pg_type` (`typtype 'd'`), `information_schema.domains`, and
  `information_schema.columns.domain_name`.
- Materialized views: `CREATE MATERIALIZED VIEW [WITH NO DATA]`,
  `REFRESH MATERIALIZED VIEW`, `DROP MATERIALIZED VIEW`, `pg_matviews`, and
  `relkind 'm'`. A write to one is refused (`42809`).
- `COMMENT ON` a table, column, index, constraint, function and more, read
  back through `obj_description` / `col_description`.
- `GRANT` / `REVOKE` on tables (recorded, as the Python server records them)
  and on other objects, `GRANT role TO role`, and
  `ALTER DEFAULT PRIVILEGES`. Missing roles (`42704`), missing relations
  (`42P01`) and bad privileges (`0LP01`) are refused.
- `TABLESAMPLE SYSTEM | BERNOULLI`, `pg_size_pretty`, `pg_size_bytes`,
  `pg_column_size`, and the relation-size functions.
- `pg_type` gains `typtype`, `typbasetype`, `typnotnull`, `typnamespace`,
  `typtypmod` and `typdefault`. `pg_namespace` uses PostgreSQL's fixed oids.
- The `xml` type: input checked for well-formedness (`2200N` / `2200M`),
  `xmlelement` / `xmlattributes`, `xmlforest`, `xmlconcat`, `xmlagg`,
  `xmlcomment`, `xmlpi`, `xmlroot`, `xmlparse`, `xmlserialize`,
  `IS DOCUMENT`, the `xml_is_well_formed*` functions, and XPath 1.0 through
  `xpath`, `xpath_exists` and `XMLEXISTS`, namespaces included.
- Every PostgreSQL text-search configuration (french, german, russian,
  spanish and 23 more), each with its Snowball stemmer and stop-word list.
  `russian` sends ASCII words to the English stemmer, as PostgreSQL does.
- `EXCLUDE` constraints over any operator (`23P01`), `MATCH FULL` foreign
  keys, and `pg_get_constraintdef` for every constraint kind.
- `pg_index` lists every index: `CREATE INDEX`, `UNIQUE` constraints and
  expression indexes, not just the primary key. Each row has a real
  `indexrelid` that `pg_class` and `::regclass` agree on, and `indkey` is an
  `int2vector` (`2 3`, subscripted from 0).
- Subqueries in a grouped query's select list, `HAVING` or `ORDER BY`,
  including an outer aggregate inside the subquery.
- A grouped query that computes over its groups (`n::text ... GROUP BY n`,
  `-n, count(*)`), which used to be refused.
- `DROP EXTENSION ... CASCADE` drops the columns of the extension's types,
  and `DROP SCHEMA ... CASCADE` drops the schema's types.
- Range and multirange operators, `box` operators, and binary results for
  `box`, `bit`, `varbit`, `regtype` and `regclass`.

#### Fixed

- `CREATE TABLE ... PARTITION OF` used to create a table with no columns,
  and every row stayed in the parent. `INHERITS` was ignored. It is now
  refused by name.
- `array_agg(DISTINCT x)` over no rows answered `{}` instead of NULL. A
  DISTINCT `array_agg` / `string_agg` ignored its `ORDER BY ... DESC` and
  `NULLS` placement. An ORDER BY that is not the argument is now PostgreSQL's
  `42P10`.
- `jsonb || jsonb` concatenated the two as text.
- `FILTER (WHERE ...)` over `generate_series` ignored the filter.
- An unknown function is `42883` with PostgreSQL's message, argument types
  included (`function foo(integer, unknown) does not exist`), rather than
  `0A000`.

- `FROM t, LATERAL (SELECT ... WHERE x = t.id) s` gave every row the same
  answer: the subquery was planned on its own, and `t.id` resolved to its
  own `id` column. A qualified column naming nothing in the FROM clause
  (`sv_c.id` inside `FROM (SELECT ... FROM sv_o)`) was silently read as the
  inner table's column. Both are now `42P01`, as in PostgreSQL.
- A select-list `t.*` returned the row as one composite column instead of
  the table's columns.
- `WHERE c.col IN (...)` with a qualified column failed with
  `column "c" does not exist`.
- `INSERT ... SELECT`, `CREATE TABLE AS` and `COPY (query) TO` over an
  aggregate or a join were refused.
- `CREATE TABLE` with an unknown column type succeeded, silently storing
  text. It is now `42704`, as in PostgreSQL.
- The catalog kept only part of a table's metadata when rewriting it.
  Table-level and constraint comments, and a named primary key, recorded by
  the Python server were erased.
- `ORDER BY 1` and `ORDER BY <alias>` over a computed column sorted by the
  column it was computed from. `SELECT n::text ... ORDER BY 1` came back in
  numeric order where PostgreSQL gives text order.
- A recursive PL/pgSQL function overflowed the worker's stack within a few
  levels and aborted the whole server. The stack now grows on demand, and
  runaway recursion is `54001`.
- A function's result is coerced to its declared return type, so a
  recursive `RETURNS numeric` function no longer overflows as an integer.
- `nextval` / `setval` are no longer rolled back with the transaction.
- A cast to an unknown type is `42704`, and `isempty('...')` with an untyped
  argument is `42725`, as in PostgreSQL.
- Text search treats every non-ASCII character as a letter, as PostgreSQL
  does under the C locale, so `a—b` and Devanagari words tokenise as they
  do there.
