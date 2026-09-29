### Ten string functions, and three that were quietly wrong

Padding a string, converting one to hex, translating characters, overlaying a
run, quoting a literal — the Rust PostgreSQL server could do none of them. Each
answered `0A000 … is not supported yet`, which is at least honest. Three others
were worse than missing: they answered, and the answer was wrong.

`split_part('a,b,c', ',', -1)` was refused outright. PostgreSQL has counted from
the end since version 14, so this rejected a form that works — and the error for
a *zero* field named the wrong rule besides ("must be greater than zero", where
PostgreSQL says "must not be zero").

`regexp_replace('abc', 'b', '\&\&')` returned `a\&\&c`. In PostgreSQL's
replacement text `\&` means the whole match, so the answer is `abbc`. The escape
was passing through as literal characters.

And `substring('abc' from '(b)')` — the regex form every client actually writes
— answered `42601 function substring does not exist with that argument list`.
That was not a missing function but a missing *overload*: the second argument's
type is what decides whether it is an offset or a POSIX pattern, and it was
always read as an offset. An error naming the user's call as malformed is worse
than one naming a gap.

Measured against PostgreSQL 14.13: the `strings` corpus went 18 divergences out
of 35 to 8, a new `strings2` corpus of 27 shapes runs clean, and `char_padding`
and `jsonpath_number` improved on the way past. Of the eight that remain, one is
a locale artifact rather than a defect (this reference server runs `lc_ctype =
C`, which does no non-ASCII case mapping), two are functions PostgreSQL 14 does
not itself have, and two need set-returning functions.

#### Added

- `lpad` and `rpad`, which count characters rather than bytes, truncate when the
  target is shorter than the input, and cannot pad at all with an empty fill.
- `to_hex`, whose width follows the argument's *type*: an `int4` `-1` is
  `ffffffff` and an `int8` `-1` is `ffffffffffffffff`.
- `translate`, which deletes the characters its target string does not cover.
- `overlay(s placing r from p [for n])`, with `n` defaulting to the length of
  `r`. A `p` below 1 answers PostgreSQL's own `22011 substring_error` — its own
  class, not the generic `22P02` a malformed value gets.
- `quote_literal` and `quote_nullable`. The pair differ only on NULL:
  `quote_literal(NULL)` is NULL, `quote_nullable(NULL)` is the four-character
  string `NULL`.
- `regexp_split_to_array`, `unistr` (all four escape spellings, with
  PostgreSQL's `42601` for a bad one) and `convert_from`.
- `substring(s FROM pattern)` and `substring(s FROM pattern FOR escape)` — the
  POSIX and SQL-standard regex forms.

#### Fixed

- `split_part` accepts a negative field, counting from the end, and its
  zero-field error is PostgreSQL's wording.
- `regexp_replace` expands `\&` to the whole match. An unknown escape such as
  `\q` still passes through as written, which PostgreSQL also does.
- `substring` with a negative length now reports `22011` rather than `22P02`,
  matching the class PostgreSQL gives.
