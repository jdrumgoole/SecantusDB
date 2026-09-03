### `character(n)` now pads the way PostgreSQL does

`char(n)` is the one string type whose stored form differs from its form in an
expression: PostgreSQL stores it blank-padded but strips those blanks on every
conversion to text. SecantusDB got the two halves from two different models —
the column path stored unpadded and padded at the wire, while a `::char(n)`
cast padded eagerly into the value — so everything downstream of a cast saw
blanks PostgreSQL had already removed.

#### Fixed

- `::char(n)` no longer pads into the value, so `'a'::char(3) || '|'` is `'a|'`
  and `length('a'::char(3))` is 1. A bare cast still reaches the client padded;
  the padding is applied at the wire, as it already was for columns.
- A cast of a non-string to `char(n)` applies the length limit:
  `123::char(2)` is `'12'`, not `'123'`. A `char(n)` target's type tag is plain
  `text`, so the number-to-text conversion returned before the char-length
  check ever ran.
- `LIKE`, `ILIKE`, `SIMILAR TO` and `~` against a `char(n)` column match the
  blank-padded value, as PostgreSQL does. They are not blank-insensitive the
  way `=` is, so a `char(5)` holding `'ab'` does not match `LIKE 'ab'` —
  SecantusDB was **returning a row PostgreSQL excludes**, in the `WHERE` clause
  as well as the select list.
- `octet_length`, `concat`, `concat_ws`, `format`, `to_json`, `to_jsonb` and a
  cast to `bytea` see the padded value, because they take it through the type's
  output function rather than as text. `length`, `upper`, `md5`, `left` and
  `position` continue to see the stripped value.
