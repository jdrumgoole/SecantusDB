### Rust PG server: `&&`, `@>` and `<@` find the elements of an array whose lower bound is not 1

An array that does not start at subscript 1 (`'[0:1]={x,y}'`, an
`array_fill` with lower bounds, an assignment below or past its bounds) was
invisible to the containment operators. `select k from x719 where a &&
'{y}'::text[]` missed the row holding `'[0:1]={x,y}'`, which PostgreSQL
15.19 finds, and `@>` and `<@` over it were false as well, in a WHERE and in
a select list alike. Beside an untyped literal the same value made the
operator unresolvable: `a && '{ab,y}'` over a `text[]` column holding one
such row was 42883 `operator does not exist: text[] && text`.

The server keeps such an array as a tagged document carrying its lower
bounds, and the containment code took that document for a scalar.
PostgreSQL's `arrayoverlap` and `array_contain_compare` walk the elements and
ignore the bounds; the operators now read the elements of a bounded array the
same way, and an untyped literal beside one is read as its array type.

#### Fixed

- `&&`, `@>` and `<@` over an array whose lower bound is not 1 compare its
  elements, as PostgreSQL does. Fixes RP-721
  (tests/backlog/test_rp_found_rp719.py::test_an_overlap_finds_an_array_with_its_own_lower_bound,
  tests/backlog/test_rp_found_rp719.py::test_containment_reads_an_array_with_its_own_lower_bound).
- An untyped array literal beside such an array resolves to its type instead
  of 42883. Fixes RP-720
  (tests/backlog/test_rp_found_rp719.py::test_an_overlap_with_an_untyped_array_literal).
