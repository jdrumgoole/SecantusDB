### Nine missing functions, and four expressions that reported the wrong type

`md5`, `btrim`, `quote_ident`, `quote_literal`, `quote_nullable`, `concat_ws`,
`starts_with`, `width_bucket` and `div` were unavailable. The error said the
function was "not supported in this context", which was misleading — they were
unreachable in every context.

Four expressions returned the right value with the wrong type, which is the
worse half because nothing reports it. `coalesce(NULL, NULL, 3)` sent the
string `'3'` typed as text where PostgreSQL sends `3` as an integer, and
`IS DISTINCT FROM` sent `'t'` as text where PostgreSQL sends a boolean — so a
driver reading the column got a string that is always truthy. `power()` and
`sign()` reported `numeric` where PostgreSQL reports double precision.

#### Added

- `md5`, `btrim`, `quote_ident`, `quote_literal`, `quote_nullable`,
  `concat_ws`, `starts_with`, `width_bucket` and `div`.

#### Fixed

- `coalesce(…)` reports the type of its arguments instead of text.
- `IS DISTINCT FROM` / `IS NOT DISTINCT FROM` report boolean instead of text.
- `power()` and `sign()` report double precision, as PostgreSQL does.
