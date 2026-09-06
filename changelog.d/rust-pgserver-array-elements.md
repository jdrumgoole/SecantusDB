### An array takes its type from its elements

`array[%s::float4]` came back as an array of *strings*. So did `array[%s]`, and
so did every array built over a parameter. The values were computed correctly
and then described wrongly: the array's type was read off the values it held,
and the pass that describes a statement to the client sees no values at all —
every parameter is NULL there — so it settled on `text[]`, and the client
decoded floats as text because the row description is what it believes.

An array's type now comes from its elements' *expressions*, which are the same
whether or not there are values to hand. Mixed numerics widen in PostgreSQL's
own order (`array[1, 1.5]` is `numeric[]`, `array[1::float4, 1.5]` is
`float4[]`), a bare NULL contributes no type at all, and the element conversion
on the way out follows the column's type rather than the first element's — which
is what turned the `1.5` in `array[1, 1.5]` into a NULL.

Two smaller things fell out. An array of dates was described as `varchar`, so a
client read back strings where PostgreSQL hands it dates; the remaining array
types now have their real oids. And a *quoted* brace was being treated as the
start of a nested array, so `'{"{"}'::text[]` answered "malformed array
literal" — only an unquoted `{` opens a sub-array, and `{` is an ordinary
member of any corpus that walks the ASCII range.

A third: two characters disappeared from any text array that carried them.
`U+0085` and `U+00A0` are whitespace to Rust's `trim` and not to PostgreSQL, so
an unquoted element that was one of them came back as the empty string — a
character in, nothing out, and invisible to any test whose alphabet is ASCII.

#### Fixed

- An array built over a parameter was described as `text[]` and its values
  handed back as strings.
- `array[1, 1.5]` turned its decimal element into a NULL.
- An array of dates, timestamps, intervals or json was described as `varchar`.
- A quoted `{` inside an array literal was read as a nested array.
- `U+0085` and `U+00A0` were trimmed out of array elements entirely.
