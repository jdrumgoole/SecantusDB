### Sorts on computed keys are priced in more shapes

`EXPLAIN` of a query that sorts one table by a computed key now prints
PostgreSQL's costs where it printed zeros: under a select list that
computes the same expression (`select a+1 from k order by a+1`), for a key
that calls a built-in function (`order by lower(b)`), and for a key over a
column the query does not return. Each key is evaluated by the scan below
the Sort, as PostgreSQL does, so its cost and width are the scan's.

#### Fixed

- A sort key equal to a computed select-list entry shares it, adding no
  cost or width; a computed select list of operators is priced under the
  Sort.
- A sort key that calls a built-in is priced at its function's `procost`,
  its width its result type's.
- A sort key reading a column the select list does not return is no
  longer declined: the scan returns the key's value, not the column.
- Fixes part of RP-1036 (tests/backlog/test_rp_explain_costs.py::test_plan[a sort on a computed key under a computed select list is priced]); a select-list entry that calls a function stays open under RP-1036.
- Filed RP-1066: `select a+1 from e order by a` prints `Sort Key: ?column?` where PostgreSQL prints `Sort Key: a`.
