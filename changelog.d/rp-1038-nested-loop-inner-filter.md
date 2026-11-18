### A nested loop over a filtered inner relation is priced

`EXPLAIN` of a join PostgreSQL runs as a nested loop, where the inner
table has a WHERE conjunct of its own -- `select e.b from e join k on e.a
<> k.a where k.b = 'v1'` -- now prints PostgreSQL's costs and its
Materialize instead of zeros. The inner scan also returns only the columns
the join and the query above it read, so a column only its own filter
reads no longer widens its rows.

#### Fixed

- A nested loop's inner side over a table with an index was refused when
  its own filter was anything but `<>` or one column's bounds, even where
  no index covers the filtered column; such a filter now leaves the
  sequential scan the only path, and the loop is priced.
- A nested loop's side scan carried every column its WHERE conjuncts read;
  it now carries those the select list and the join's clauses read
  (`build_base_rel_tlists`), as PostgreSQL's widths show.
- Fixes RP-1038 (tests/backlog/test_rp_explain_costs.py::test_plan[a nested loop over a filtered inner relation is priced])
- Filed RP-1068: an inner index qual (`where k.a = 5`) is still unpriced, and PostgreSQL makes that side the outer.
