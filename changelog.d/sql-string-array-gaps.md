### Nine divergences a broad sweep against PostgreSQL turned up

A corpus of ordinary SQL run against both servers and diffed — not read off the
backlog. Three were **silently wrong answers** rather than errors.

`trim(both 'x' from 'xxabxx')` answered `'xxabxx'`: the trim characters and the
position were both ignored, and every spelling ran a plain `str.strip()`. Only
the SQL keyword form was affected — `btrim` / `ltrim` / `rtrim` take their
characters as an ordinary second argument and were always right.

`substr('abcdef', -1, 3)` answered `'abc'`. The start was clamped to 1 and the
length counted from there; PostgreSQL counts from the *original* start, so
positions -1, 0 and 1 leave just `'a'`.

`unnest('{1,2,3}'::int[])` handed a driver `'1'`, `'2'`, `'3'` — the elements
were typed `any` and went out as text.

#### Fixed

- `TRIM([LEADING|TRAILING|BOTH] [chars] FROM string)` honours both the
  character set and the position.
- `substr()` counts the length from the original start, and a negative length
  is `22011 negative substring length not allowed`.
- `unnest()` without a FROM types its elements: from the cast where there is
  one, otherwise from the values, with PostgreSQL's own rule that an
  `ARRAY[…]` of integer literals is `integer` unless one does not fit.
- A **NULL array** is empty in a `||` concatenation, so `NULL::int[] || 9` is
  `{9}` where it used to be NULL — while a NULL of any other type still makes
  the whole `||` NULL, and a NULL *element* stays a NULL element. A column's
  array-ness comes from the type-checking pass, a cast or `ARRAY[…]` from the
  node itself.
- `to_hex()`, `make_date()`, `make_time()` and `make_timestamp()`, each of
  which was `0A000 function … is not supported` — and two of them under a name
  the user never wrote, because sqlglot renames them.

#### Still divergent

`SIMILAR TO`; `date_part()` returns `numeric` where PostgreSQL returns
`float8` (sqlglot parses it and `extract`, which *is* numeric, to the same
node); and two-argument `log()` returns `float8` where PostgreSQL returns a
`numeric` whose scale comes from an estimator in `log_var` that has not been
ported.
