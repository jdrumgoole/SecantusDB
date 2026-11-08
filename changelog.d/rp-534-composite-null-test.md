### Rust PG server: a composite value's IS NULL in a WHERE or HAVING tests its fields

`select count(*) from fk where k is not null`, over a column `k` of a
composite type, counted every row whose value was not null, including
`row(1, null)`. PostgreSQL's `ExecEvalRowNull` makes a row IS NULL when every
field is null and IS NOT NULL when none is, so `row(1, null)` is neither. The
select list already answered that way. A WHERE or HAVING lowered the test to
the storage filter `{k: null}`, which only tests the value itself.

#### Fixed

- `x IS [NOT] NULL` and `NOT (x IS [NOT] NULL)` over a column of a composite
  type, or of type `record`, are now evaluated per row and read the fields.
  That covers a table's composite column, a FROM-subquery's row output
  (`(select fs x from fs) s where x is not null`) and a grouped whole row
  (`group by fs having fs is null`). PostgreSQL never uses an index for such
  a test either: `match_clause_to_indexcol` refuses an `argisrow` null test.
  DELETE, UPDATE, CHECK constraints and partial-index predicates take the
  same path.

Fixes RP-534 (tests/backlog/test_rp_found_rp532.py::test_composite_null_test_in_a_clause),
pinned by `secantus-pgplan`'s `composite_null_test_is_evaluated_per_row`.
