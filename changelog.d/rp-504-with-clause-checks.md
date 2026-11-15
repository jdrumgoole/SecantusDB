### A WITH is checked as PostgreSQL's transformWithClause checks it

A WITH clause now answers the errors PostgreSQL's parse analysis raises for
it, whether or not the statement reads the item at fault. A name given twice
is 42712 at the second, an alias list longer than the item's columns is
42P10 `WITH query "w" has 1 columns available but 2 columns specified` at
the item, a reference to an item written later carries PostgreSQL's hint to
use WITH RECURSIVE or re-order the items, and the WITH of a subquery
expression is analysed as a statement's is.

#### Fixed

- Rust PG server: `with w as (select 1 a), w as (select 2 a) ...` is 42712
  (the first item used to win); `with w(x, y) as (select 1) ...` is 42P10 at
  6 (42601 `table "w" ...` with no position when read, nothing when not);
  a forward reference's 42P01 has the HINT `Use WITH RECURSIVE, or re-order
  the WITH items to remove forward references.`; `select (with w as (select
  nosuch) select 1)` is 42703 at 27 (it answered 1). An unread item's
  literal input and a correlated subquery's unread item stay open under
  RP-504.
- Fixes RP-504 in part (tests/backlog/test_rp_fromless_analysis.py::test_with_clause_checks)
