### `substring(text from pattern)` crashed, and `LIKE` ignored its default escape

`substring('abc123' from '[0-9]+')` — the pattern-matching form — reported an
internal error. The pattern was being read as a starting position.

`'a_c' LIKE 'a\_c'` was false. Backslash is PostgreSQL's default escape
character in `LIKE`, so the backslash should make the `_` match literally;
SecantusDB only honoured an escape when one was written out with `ESCAPE`.
`ESCAPE ''`, which genuinely turns escaping off, still does.

Three more expressions reported the wrong type, so their values arrived as
text: `BETWEEN` and `EXISTS` sent `'t'`/`'f'` instead of booleans, and a
scalar subquery sent the string `'1'` instead of an integer.

#### Fixed

- `substring(text FROM pattern)` returns the first capture group when the
  pattern has one, the whole match when it does not, and NULL when it does not
  match — instead of failing.
- `LIKE` treats backslash as its escape character by default, as PostgreSQL
  does.
- `BETWEEN`, `EXISTS` and scalar subqueries report their real types.
- `array_replace()` keeps the array's type instead of rendering it as text.

#### Added

- `regexp_match`, `regexp_split_to_array`, `string_to_array`, `array_replace`.
