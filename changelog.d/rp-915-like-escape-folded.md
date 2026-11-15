### A LIKE's ESCAPE clause is folded into its pattern

`b LIKE 'v1!%' ESCAPE '!'` is a `like_escape` call over two constants, which PostgreSQL folds into the pattern rewritten for the default backslash escape before it estimates the match: EXPLAIN shows `Filter: (b ~~ 'v1\%'::text)` with `patternsel`'s row estimate. The Rust PostgreSQL server now folds it the same way, on a scan and in a nested loop's Join Filter, and raises PostgreSQL's `invalid escape string` at plan time for an escape longer than one character. String constants in EXPLAIN and view definitions now print as ruleutils prints them, a backslash as it is and no `E'...'` prefix.

#### Fixed

- EXPLAIN folds `like_escape` of two constants, and prints constants with `simple_quote_literal`.
  Fixes RP-915 (tests/backlog/test_rp_explain_costs.py::test_plan[a like with an escape clause is a folded pattern])
