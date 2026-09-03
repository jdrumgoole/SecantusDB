### An eighth SQL sweep: four silently wrong answers, and a whole type that answered NULL

263 statements run against PostgreSQL 14.13 through the same `psycopg` client
on both sides — so client-side type mapping is identical and every difference
is the server's. The four worst findings all returned a *plausible* answer
rather than an error.

**Every navigation over a `json` value answered NULL.** The `json` type keeps
the client's exact text — whitespace, key order and duplicate keys preserved,
which is what separates it from `jsonb` — so a `::json` value arrives as a
`str` subclass. The `->` / `->>` / `#>` / `#>>` walker descends `dict` and
`list` only, so it fell straight through to "not a container" and answered NULL
for the entire type. `'{"a":1}'::json -> 'a'` was NULL where PostgreSQL says 1.

**`ORDER BY b.id` sorted by `a.id`.** Two joined tables routinely project
same-named columns; the order term was resolved by its *bare* name against the
output list, which finds the first of them. In a `RIGHT JOIN` it also misplaced
the unmatched rows, because their `a.id` is NULL and NULLs sort to one end.

**`SELECT jsonb_each(x)` returned no rows at all.** The SELECT-list record-SRF
expansion was written for `_pg_expandarray` and treats the argument as an
array; `jsonb_each`'s argument is an object, so it expanded to zero elements
and the statement answered an empty result.

**`ORDER BY 99` was accepted and ignored.** Each planning path gates on
`1 <= n <= len(select list)` and, when that fails, leaves the literal alone —
which sorts by a constant, i.e. not at all.

#### Fixed

- **`json` (non-`b`) navigation.** `->`, `->>`, `#>`, `#>>` and
  `json_extract_path` all descend a `json` value again instead of answering
  NULL.
- **`#>` keeps `jsonb`; `#>>` returns `text`.** The type inference named `#>` in
  its comment but tested only the `JSONExtract` classes — `#>` parses to
  `JSONBExtract`, which is *not* a subclass — so `#>` went out under oid 25 and
  `'{"a":{"b":[1,2]}}'::jsonb #> '{a,b}'` rendered the PostgreSQL array literal
  `{1,2}` instead of the jsonb `[1, 2]`.
- **`ORDER BY <n>` out of range** raises `42P10` (including `0` and a negative
  ordinal, which parses as `Neg(Literal)` and never reached the range gate).
- **A qualified `ORDER BY` term** matches the select list by its full text
  before falling back to the bare column name, so `ORDER BY b.id` no longer
  sorts by `a.id`.
- **`JOIN ... ON a.id = b.id - 1`.** The fast-path "is this a simple equality?"
  detector *raised* instead of answering no, so any ON term that is not a bare
  column was a fatal `0A000 ON must compare columns` — even though the general
  pipeline form lowers arithmetic perfectly well.
- **`SELECT jsonb_each(x)` / `jsonb_each_text(x)`** expand to one row per
  member, in the composite and the `(...).key` / `(...).value` field forms; the
  composite renders a `jsonb` field as JSON (`(b,"x")`, not `(b,x)`).
- **An incomparable pair** (`ARRAY[1,2] > 1`, `'{"a":1}'::jsonb > 1`) raises
  `42883` naming the operator, rather than leaking a Python `TypeError` to the
  client as `XX000 internal error`.
- **`initcap`** treats a digit as part of a word, as PostgreSQL does:
  `initcap('a1b c')` is `A1b C`, not `A1B C`.
- **`quote_ident`** quotes a keyword that is not UNRESERVED
  (`quote_ident('select')` → `"select"`), and **`format`'s `%I`** is now that
  same rule instead of always quoting — `format('%I', 'tbl')` is a bare `tbl`.
  A NULL to `%I` raises `22004`.
- **`LIKE ... ESCAPE`** types as `boolean`. The ESCAPE clause wraps the
  predicate in a node that is not itself a boolean class, so adding it to a
  working `LIKE` flipped its column from oid 16 to oid 25 and sent a `'t'`.
- **A timestamp literal's fractional seconds, at any width.** The support
  matrix disagreed with itself in both directions and neither half matched
  PostgreSQL: before Python 3.11 `fromisoformat` accepted *only* 3 or 6
  fractional digits, so `TIMESTAMP '2020-01-15 10:30:45.5'` — a perfectly good
  literal — raised on 3.10 while parsing on 3.12; from 3.11 it accepts any
  width but *truncates* beyond six digits, where PostgreSQL *rounds*
  (`.1234567` → `.123457`). Normalising the fraction before the parse makes
  every supported Python answer the same microseconds, and the same ones
  PostgreSQL does. Only the CI matrix could show this — a local 3.12 run sees
  neither half.
- **An unresolvable function name** answers `42883 function f(...) does not
  exist` rather than `0A000 ... is not supported in this context`, which
  claimed the call site was the problem when the name was unreachable
  everywhere. A *set-returning* function in scalar position keeps `0A000` and
  now says what the actual limit is.

#### Added

- **`SIMILAR TO` / `NOT SIMILAR TO`**, with `ESCAPE`. SQL's regex flavour is
  LIKE's wildcards plus `| * + ? {} () []`; every other character is literal,
  so `'abc' SIMILAR TO 'a.c'` is false.
- **Quantified comparisons under every operator.** `ANY` / `ALL` worked only
  under `=` and `<>`; `1 < ALL(ARRAY[2,3])` was an outright error. The set form
  (`x = ANY (SELECT ...)`) works too, and the three-valued rules match
  PostgreSQL — including that an empty array settles the answer before the
  needle is looked at (`NULL = ALL('{}')` is true).
- **`DISTINCT` inside `array_agg` / `string_agg` / `jsonb_agg`.** PostgreSQL
  dedupes by *sorting*, so the result comes back ascending even with no
  `ORDER BY` written; an `ORDER BY` naming anything but the argument raises
  `42P10`, as it does there.
- **`jsonb_extract_path`, `jsonb_extract_path_text`, `jsonb_path_query_first`,
  `array_to_json`, `trim_array`.** `sqlglot` folds only the `json_` spelling of
  the extract-path pair into a dedicated node, which is why one of each pair
  worked and the other did not.
- **`extract`**: `isoyear`, `julian`, `microseconds`, `milliseconds` (the last
  two fold the seconds in, as PostgreSQL does). **`date_trunc`**: `decade`,
  `century`, `millennium`, `milliseconds`, `microseconds` — centuries and
  millennia start at year 1, so 2026 truncates to 2001.
- **`generate_series` over `date` / `timestamp` bounds with an interval step.**
  The bounds arrive as canonical text, so the temporal overload was rejecting
  its own arguments; `date` bounds type as `timestamptz` and `timestamp` bounds
  as `timestamp`, which is how PostgreSQL resolves the overloads.
