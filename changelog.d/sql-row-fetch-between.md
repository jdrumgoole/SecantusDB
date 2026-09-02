### Two more internal errors, and three wrong answers, from a second sweep

`(1,2) < (1,3)` was `XX000`. A record rides as a dict of `f1..fN`, and a dict
has no `<` — equality worked, so only the ordering comparisons failed.

`FETCH FIRST 2 ROWS ONLY` — the SQL-standard spelling of `LIMIT` that
PostgreSQL accepts — was `XX000` twice over. It parses as `exp.Fetch`, whose
count lives in `count` rather than `expression`, and the "unsupported" error
built from that `None` then raised `AttributeError` on `None.sql()`. So the
query could not even say why it failed.

`3 BETWEEN SYMMETRIC 5 AND 1` answered FALSE: the keyword was parsed and then
ignored, so every reversed-bound test was wrong.

#### Fixed

- Records compare field by field, left to right.
- `FETCH FIRST n ROW[S] ONLY`, with or without an `OFFSET`.
- `BETWEEN SYMMETRIC` orders its bounds first. Plain `BETWEEN` still does not.
- `jsonb ? 'k'` and its `?|` / `?&` siblings work in a SELECT list — they
  reported `function jsonb_contains() is not supported`, a name the user never
  wrote, and for the two-key forms a name mangled out of the node class. They
  worked inside a `WHERE` all along. An object is asked about its keys, an
  array about its string elements, and a jsonb string about equality, which is
  PostgreSQL's rule.
- Every containment and key-existence operator (`@>`, `<@`, `&&`, `?`, `?|`,
  `?&`) types as `boolean`. They typed as `text`, so a driver was sent `'t'`
  under oid 25 where PostgreSQL sends a boolean under oid 16.
