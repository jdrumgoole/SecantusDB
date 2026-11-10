### Rust PG server: a PL/pgSQL assignment with no assignment cast converts through text

PL/pgSQL coerces an assigned value with `COERCION_PLPGSQL`. It uses an
implicit or assignment cast, never an explicit-only one. When neither
exists, it converts through the source type's output function and the
target type's input function. The Rust PostgreSQL server used the
explicit cast instead, so `x int := true` stored 1 and `x bool := 5`
stored true, where PostgreSQL 15.19 raises 22P02.

#### Fixed

- `x := true` into an `int`, `select true into x`, `x bool := 5`,
  `x date := 5`, `x int[] := array[true]`, `x inet := 5` and the like go
  through the source's output text (`t`, `5`, `{t}`) and the target's
  input. So they raise PostgreSQL's input error: `invalid input syntax for
  type integer: "t"`, 22007 for a date, `"5" is not a valid binary digit`
  for a `bit(3)`. Some used to store a value, and others were 42846
  `cannot cast`. Domains, `varchar(n)` / `bit(n)` lengths and NULL are
  handled as before. Where an implicit or assignment cast exists (`int8`
  to `int4`, `numeric` to `int4`, `bool` to `text` as `true`), it is
  still the one used.
- The conversion's error is framed as on PostgreSQL. In a `:=` over a
  constant whose I/O functions are immutable, the conversion folds while
  the query is planned, so it carries `PL/pgSQL assignment "x := true"`.
  Through a stable function (`date_in`, `range_in`, `cash_out`) or over a
  variable, it carries only the `at assignment` line. An `INTO`'s cast runs
  outside its query, so its error carries no `SQL statement` frame, for
  every cast `INTO` makes.

Fixes RP-712 (tests/backlog/test_rp_found_rp694.py::test_assignment_with_only_an_explicit_cast_converts_through_text,
tests/backlog/test_rp_found_rp694.py::test_conversion_through_text_is_framed_as_postgres_frames_it,
tests/backlog/test_rp_found_rp694.py::test_assignment_cast_is_used_where_one_exists)

#### Found

- RP-749: `numeric` out of `int4`'s range quotes the value in its 22003
  message.
- RP-750: a `jsonb` input error carries no `JSON data, line 1: ...`
  CONTEXT.
- RP-751: a `money` variable prints its bare number in a RAISE (`5.25`,
  not `$5.25`).
- RP-752: an assignment's 22001 for an overlong `varchar(n)` value has no
  `PL/pgSQL assignment` frame.
- RP-753: an assignment's literal input error (`x int := 'abc'`) carries
  no internal query.

All in `tests/backlog/test_rp_found_rp712.py`.
