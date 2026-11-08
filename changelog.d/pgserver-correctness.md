### The Rust PostgreSQL server closes forty-seven measured gaps to PostgreSQL 16

A campaign of differential probes against PostgreSQL 16.15 — run the same SQL on
both servers, compare every value, error code and error position — found a long
tail of places where the Rust PostgreSQL server answered differently from the
real thing, and this release closes forty-seven of them. The largest are
structural: constraints (`NOT NULL`, `CHECK`, `FOREIGN KEY`, `UNIQUE`) are now
stored in the catalog and enforced, and reflected through `pg_constraint`,
`pg_index`, `pg_indexes`, `pg_get_constraintdef` and `pg_get_indexdef`; column
`DEFAULT` expressions (`now()`, `gen_random_uuid()`, `nextval(...)`) are
evaluated per row; sequences are created, altered and dropped for real, drawn
once per row, and no longer roll back with the transaction; `ALTER TABLE`
gained `RENAME TO`, `ALTER COLUMN TYPE`, `ADD` / `DROP` / `RENAME CONSTRAINT`,
`DROP COLUMN` and `RENAME COLUMN` with constraints following the column; and
`SELECT *` / `t.*` over a `JOIN` or a comma `FROM`, derived tables, common table
expressions, subquery expressions, `PREPARE` / `EXECUTE`, and `RETURNING` over
n-way joins all work.

The rest is the SQL surface a client reaches for without thinking: the
date/time family (`extract` / `date_part` / `date_trunc`, `age`, datetime
arithmetic that keeps microseconds on stored timestamps, the `now` / `today` /
`tomorrow` / `yesterday` / `epoch` input keywords, zone abbreviations and bare
date offsets, `timestamptz::timestamp` in the session zone, the `(p)` precision
typmods), `money`, bit strings, `real` as genuine single precision, `char(n)`
padding in every output form, declared string widths enforced on insert, the
`numeric(p, s)` typmod, `LIKE` / `ILIKE` / `SIMILAR TO` / POSIX regexes, integer
bitwise and power operators, constant-array subscripts, `translate`,
`split_part` with a negative position, `abs(-2147483648)` overflowing as
PostgreSQL overflows, `concat(true)` rendering `t`, and SQL value functions
anywhere in an expression. Operator-resolution and assignment errors now carry
PostgreSQL's code and the position of the offending token, and casts to an
unknown type are `42704` rather than "not supported".

#### Added

- `crates/secantus-pgplan`: `bits.rs` (bit strings), `collate.rs` (text
  ordering), `datetime_fns.rs` (`extract` / `date_trunc` / `age` and the
  datetime arithmetic), `deparse.rs` (expression rendering for
  `pg_get_constraintdef` / `pg_get_expr` / `information_schema`); `money`,
  `real` single-precision arithmetic and text, `numeric(p, s)` typmod
  enforcement, range operators and `RangeSubselect`, JSON constructors,
  `to_json`, `quote_literal`, `array_to_string`, array shape functions.
- `crates/secantus-pgcatalog`: `NOT NULL` / `CHECK` / `FOREIGN KEY` / `UNIQUE`
  constraints, column `DEFAULT` expressions and sequences in the catalog
  document, in the same document shape the Python server writes (a
  Rust-only `default_text` display key is written only when there is one).
- `crates/secantus-pgserver`: constraint enforcement on INSERT / UPDATE /
  DELETE (`23502` / `23514` / `23503` / `23505`), `pg_constraint` / `pg_index`
  / `pg_indexes` virtual tables, `CREATE` / `ALTER` / `DROP SEQUENCE` and the
  sequence functions, `ALTER TABLE` column and constraint actions, `DROP ...
  IF EXISTS` notices and the missing-object codes for every `DROP` form,
  `PREPARE` / `EXECUTE`, CTEs, derived tables and subquery expressions, `*`
  expansion over joins, `JOIN USING` / `NATURAL` merging the key columns,
  every outer-join kind with any `ON`, `DISTINCT ON` and `ORDER BY <position>`
  over aggregates, in-aggregate `ORDER BY` over an expression, `pg_sleep` in
  any expression, and `psql`'s `\d` describe queries.

#### Fixed

- `min` / `max` of a boolean, `box = box`, `float4` text (`float4out`, not the
  shortest round-trip form), `inet[]` / `cidr[]` text, `::text` of a
  `tstzrange` / `timestamptz[]` keeping the offset, an `ARRAY[...]` mixing a
  typed element with an unknown literal, the INSERT half of the assignment
  rule (`jsonb` column refuses `42`), a bare boolean `WHERE` and the `42804`
  for anything else, a float column's `NaN` sorting and comparing where
  PostgreSQL puts it, `timestamp + interval` on a stored timestamp keeping its
  microseconds, and `nextval` drawn once per row rather than once per
  reference.
- Casts: a `bytea` value (a bound binary parameter, say) casts to text as
  its `\x` hex form and to itself, and every other target is `42846 cannot
  cast type bytea to ...` on the declared type -- no more `22P02` carrying
  the Rust `Debug` rendering of the bytes; a scalar cast to an array type is
  `42846 cannot cast type integer to integer[]`; a cast to an array of a
  MODIFIED type applies the modifier per element (`array[1.234]::numeric(5,2)[]`
  is `{1.23}`, `array['abc']::varchar(2)[]` is `{ab}`) and is described with
  it; and an unquoted reserved word is no longer accepted as a `regtype` name
  (`'order'::regtype` is the syntax error PostgreSQL raises; `'"order"'` resolves).
- Assignment errors are positioned as PostgreSQL positions them: a literal
  the column's type cannot read (`UPDATE t SET i = 'abc'`, `INSERT … VALUES
  ('2020-13-45')`) fails at plan time pointing at the literal, `INSERT has
  more expressions than target columns` points at the first excess
  expression and `… more target columns than expressions` at the first
  unfilled column. A scalar subquery is typed by its output column, not by
  the value it yields: `SET i = (select 'x')` and `SET i = (select t FROM …)`
  are `42804 column "i" is of type integer but expression is of type text`
  at the `(`, where the value used to be coerced (or accepted) at run time.
- An unknown column is reported the way PostgreSQL reports it: `42703` now
  carries the position of the column's first mention, a qualified reference
  says `column a.nosuch does not exist` (unquoted, in the written spelling,
  positioned at the qualifier) rather than `column "nosuch" does not exist`,
  and a JOIN's `ON a.nosuch = b.i` fails when the join is planned rather than
  after its sides are read. A qualified `WHERE a.i IS NULL` resolves the
  column `i`; it used to resolve the qualifier and answer `column "a" does
  not exist`.
- Boolean contexts are typed and positioned as PostgreSQL types them. A
  non-boolean term in `HAVING`, `JOIN/ON` or a join's `WHERE` is `42804`
  naming the INNERMOST connective it is an argument of (`argument of AND
  must be type boolean, not type integer`, not `of WHERE`) at the leaf --
  including an arm the row evaluator would have short-circuited past. A
  string literal there is read as a boolean (`having 'true'` keeps the
  group, `'x'` is `22P02` at the literal); a `NULL` is UNKNOWN, so
  `where null`, `on null` and `having not null` keep no row and a `LEFT
  JOIN ... ON null` keeps its left rows; and `HAVING` evaluates in three
  values, so `NOT (max(t) FILTER (WHERE ...) = 'x')` over an empty filter is
  UNKNOWN rather than true. A column two join sides supply is `42702` at
  its first unqualified mention. A column that is neither grouped nor
  inside an aggregate is `42803` at the column wherever it is read (select
  list, `HAVING` under a connective or a function, `ORDER BY`), named by
  the FROM alias when there is one (`column "a.t" must appear ...`), and a
  qualified `a.t` is that error too rather than an unknown column.
- `IS [NOT] DISTINCT FROM` is the null-safe comparison in every clause --
  select list, `WHERE`, `ORDER BY`, `HAVING`, bare constants -- rather than
  a refusal, and in `HAVING` it is no longer read as a plain `=` (which kept
  the row `max(t) is distinct from 'x'` should drop). `HAVING` over a NULL
  test or a `CASE` that contains an aggregate (`(max(t) = 'y') is not
  null`, `(i = 1 or null) is null`) evaluates the expression, and an
  unknown column there is `42703` at the column, never a grouping error.
- A call to a function PostgreSQL does not have is `42883 function
  nosuch(integer) does not exist`, with the "No function matches the given
  name and argument types" hint and the call's position, wherever the call
  sits (select list, `WHERE`, `HAVING`, `ORDER BY`, `FILTER`, a subquery,
  `UPDATE`, `DELETE`); the argument types are the ones PostgreSQL resolves
  (an untyped literal is `unknown`, `i * 2.5` is `numeric`, `row(1, 2)` is
  `record`, `array[1, 2]` is `integer[]`, a `CASE` of untyped literals is
  `text`). It had been `0A000 function nosuch() is not supported yet`
  without a position on every path, and over a table column the statement
  planned and failed per row. A real PostgreSQL function this server has
  not implemented -- the planner carries the 3,307 names of PostgreSQL 16's
  `pg_catalog`, `hstore` and PostGIS -- and a session-created `LANGUAGE
  internal` function both stay the honest `0A000`.
- A `$n` the statement was given no value for is `42P02 there is no
  parameter $n` at the reference, on every path -- a scalar subquery, a
  grouped select, a function argument -- and resolved where PostgreSQL
  resolves it: after an unknown relation or a column fault earlier in the
  text, before the function lookup around it, a grouping fault or any
  evaluation. It had been unpositioned, `0A000 SubLink is not supported
  yet` inside a subquery, and a grouped `select $1 from t group by c`
  answered a row.
- `INSERT ... SELECT` checks a literal, cast or scalar-subquery select item
  against its target column at plan time the way a `VALUES` item is checked:
  an untyped literal the column cannot read is `22P02` at the literal, and a
  typed item with no assignment cast is `42804 column "i" is of type integer
  but expression is of type text` at the item. The literal had failed
  unpositioned at run time, and a subquery was read as the value it yields.

- A cast in a boolean context is typed before it is evaluated: a constant
  `'abc'::text`, `1::int` or `CAST(... AS int)` in `WHERE` is `42804 argument
  of WHERE must be type boolean` at the operand (`of AND` as a conjunct), a
  `::bool` one filters, and `'x'::bool` is `22P02` -- over a table, over `FROM
  generate_series(...)`, and over a FROM-less series, whose `WHERE` and
  `HAVING` are now evaluated as the constant predicates they can only be
  (`select generate_series(1, 3) where false` is `[]`; it had been refused
  `0A000`). No column is in scope there, so the output alias in `WHERE` or
  `HAVING` is `42703` at that reference -- after a `GROUP BY` naming the same
  alias, which resolves. An assignment `CAST` is positioned at the `CAST`,
  not at the literal inside it.
- A sequence the catalog lacks is `42P01 relation "<name>" does not exist`
  at the string literal that named it, spelled as written (`public.nosuchseq`),
  wherever `nextval` / `currval` / `setval` sits -- select list, `WHERE` (bare,
  `::int`, `CAST`), an INSERT value, an UPDATE assignment -- and resolved
  where PostgreSQL resolves it: before the boolean-context `42804` around it,
  after a syntax error, an unknown FROM relation or a column / literal fault
  earlier in the text. It had been raised unpositioned after planning, so a
  `WHERE nextval(...)` was `42804` instead.
- A syntax error carries PostgreSQL's position: `42601 syntax error at or
  near "<token>"` at the token, `at end of input` one past the text,
  `unterminated quoted string` at the opening quote, counted in characters
  over the whole query string, in the simple and the extended protocol. The
  position libpg_query reports had been dropped.
- A CASE's arms, COALESCE / GREATEST / LEAST's arguments, NULLIF's operands,
  the columns of a UNION / INTERSECT / EXCEPT and of a VALUES list resolve to
  ONE type by PostgreSQL's `select_common_type`: the first typed input is
  the candidate, an untyped literal is read as that type at parse time (so
  `case when i = 1 then 1 else 'a' end` is `22P02` at the literal even when
  the arm is never taken, and `select 1 union select '2'` is two integers),
  a different type category is `42804 <construct> types X and Y cannot be
  matched` at the offending input, a typed input with no implicit cast is
  `42846 <construct> could not convert type X to Y`, and within a category
  the candidate widens along an implicit cast (`int` beside `numeric` is
  numeric, so `select 1 union select 1::numeric` is ONE row; `date` beside
  `timestamptz` is timestamptz; `varchar` beside `name` is `name`, because
  `pg_cast` has name-to-varchar as an assignment cast only). Each arm had
  been typed on its own: `'2'` beside `1` came back as text, `'a'` never
  failed, the UNION kept the left side's type and `null::text union select
  1` was accepted. NULLIF also widens its operands (`nullif(1, 1.0)` is
  numeric) and refuses two categories with no implicit cast as `42883
  operator does not exist: integer = boolean`.
- A call's arguments are resolved before the function is looked up, so
  `nosuch(case when i = 1 then 1 else 'a' end)` is the argument's `22P02`,
  not the call's `42883`.
- A call to a function PostgreSQL DOES have resolves against its overloads
  by PostgreSQL's own rules (`func_select_candidate`), so the selected
  overload both declares the result type and reads each argument as its
  parameter type: `abs(1::int2)` is a `smallint` and `abs(1.5)` a `numeric`,
  `mod(i, 3::int8)` over an `int` column is a `bigint`, `ceil(i)` is a
  `double precision`, `abs('1')` parses the literal as the float8 `1` and
  `power(2, 3)` widens both operands to double precision. Arguments matching
  no overload are `42883 function md5(integer) does not exist` at the call,
  an untyped argument no single candidate claims is `42725 function
  trunc(unknown) is not unique`, and a polymorphic parameter handed an
  untyped literal is `42804 could not determine polymorphic type because
  input has type unknown` -- each with PostgreSQL's hint. Before this, a
  wrong-typed call planned and then either answered from the wrong overload
  (`abs(1::int2)` came back an `integer`, `mod(i, 2)` a `numeric`) or failed
  per row with a Rust-shaped message. `pi()` is implemented too.

- `generate_series` resolves against its own overloads too, so the bounds
  fix the ELEMENT type: a series past 2^31 is a `bigint` one (it used to
  truncate into NEGATIVE int4 values -- `generate_series(3000000000,
  3000000002)` answered `-1294967296, ...`), a fractional step makes a
  `numeric` series that walks by repeated addition with PostgreSQL's scales
  (`1, 1.5, 2.0, 2.5, 3.0`, refused `0A000` before), and a `timestamp` /
  `timestamptz` pair with an interval step walks the calendar (January 31st
  plus a month is February 29th) where it used to read the interval as an
  integer and fail `22P02`. Bounds no overload takes are the resolver's
  refusal: three small-int parameters -- what psycopg sends for
  `generate_series(%s, %s, %s)` -- are `42725 ... is not unique`, as they are
  on PostgreSQL, where this server answered rows.
- A fault in an `ORDER BY` / `GROUP BY` / `DISTINCT ON` item is reported AT
  the item, and names its own clause: a constant that is not a position is
  `42601 non-integer constant in GROUP BY` (it had been a `42803` grouping
  error about the select list), a position outside the select list is
  `42P10 <clause> position N is not in select list`, and a `SELECT DISTINCT`
  whose sort term it does not project is `42P10` -- all of them
  unpositioned before, on the plain, aggregate, set-operation and subquery
  paths alike. `DISTINCT ON` now also resolves a POSITION (`DISTINCT ON
  (1)`), implies the sort on its keys when the query has no `ORDER BY` of
  its own (PostgreSQL's sort-then-unique order, not the rows' own), and must
  lead the `ORDER BY`: outside an aggregate that rule was unchecked, so a
  query PostgreSQL refuses answered rows.

#### Testing

- `tests/test_rust_pgserver_slice.py`: ninety-four new tests, each asserting
  values measured on PostgreSQL 16.15.
- `tools/probes/pg_differential.py`: the probe harness the campaign ran on.
- `crates/secantus-pgcatalog/src/golden.rs`: the catalog document a table
  without expression defaults writes is still byte-identical to the Python
  server's.
