### A `text` column is not written to a domain-array column by `INSERT ... SELECT`

`text` has no assignment cast to an array of a domain, so PostgreSQL
refuses `insert into t(d) select '{1}'::text` into a `pint[]` column (and
the same bare literal under a UNION, and a `text[]` or a `NULL` typed
`text`) with 42804 `column "d" is of type pint[] but expression is of type
text`, before a row is read. The Rust PostgreSQL server skipped the check
for a target type it could not name, which an array of a domain is, and
read the text as the array. It now looks through the domain to its base,
as `find_coercion_pathway` does, so `int[]`, `bigint[]` and `numeric[]`
sources are still coerced and the others are refused.

#### Fixed

- `INSERT ... SELECT` into an array-of-domain column applies the
  assignment cast rules of the domain's base type, as on PostgreSQL 15.
  Fixes RP-1048 (tests/backlog/test_rp_found_rp552.py::test_insert_select_text_into_domain_array)
