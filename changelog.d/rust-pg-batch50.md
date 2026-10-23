### Rust PostgreSQL server: ANALYZE builds extended statistics

ANALYZE now builds what each `CREATE STATISTICS` object asks for, as
PostgreSQL 15.19's `BuildRelationExtStatistics` does.
`pg_statistic_ext_data`, `pg_stats_ext` and `pg_stats_ext_exprs` show the
results; before, they were refused by name.

#### Fixed

- Fixes most of RP-099
  (`tests/backlog/test_rp_batch26.py::test_scenario[ANALYZE computes extended statistics]`,
  and `tests/backlog/test_rp_batch50.py::test_scenario[...]`:
  `ndistinct, dependencies and MCV lists over columns`, `expression
  statistics`, `ANALYZE of some columns builds only what they cover`,
  `numeric, wide, empty and a small target`, `an MCV list's bytes over
  numeric and dates`, `a partitioned table's statistics are inherited` and
  `an inheritance parent's statistics are built both ways`):
  - **ndistinct.** Each combination of two or more of an object's columns
    and expressions, by the Duj1 estimator. Expressions are numbered -1,
    -2, ... after the columns.
  - **dependencies.** Each functional dependency's degree, in PostgreSQL's
    order, with those of 0 left out. A value over 1 kB is left out of the
    sorted sample, as PostgreSQL leaves it.
  - **MCV lists.** The most common combinations, ordered by frequency as
    PostgreSQL's quicksort leaves them, with their base frequencies.
    `stxdmcv` holds PostgreSQL's own serialised bytes for integers, floats,
    booleans, text types, numerics and dates, and `pg_mcv_list_items`
    reads them back.
  - **Expression statistics.** Each expression's `pg_statistic` row, as a
    column's, with a computed value's 4-byte varlena header in its width.
    `stxdexpr` prints them as a `pg_statistic[]`.
  - **Which objects ANALYZE builds.** A partitioned table's statistics are
    inherited. An inheritance parent's are built both from its own rows and
    from its tree's. `ANALYZE t (a)` builds only the objects whose columns
    it analyses, and warns of each other one, in creation order.
- Fixes RP-197
  (`tests/backlog/test_rp_batch50.py::test_scenario[pg_get_statisticsobjdef and its siblings]`):
  `pg_get_statisticsobjdef`, `pg_get_statisticsobjdef_columns` and
  `pg_get_statisticsobjdef_expressions` were refused. They now print as
  PostgreSQL does: the kinds when not all of them, the definition's
  expressions pretty, the `_expressions` list as `pg_get_expr` prints.
- Fixes RP-198
  (`tests/backlog/test_rp_batch50.py::test_scenario[a real is cast to text as a real]`):
  a `real` (or `real[]`) cast to a string type printed as the double it is
  kept in (`0.4000000059604645`), in a table and in `pg_stats` alike. It now
  prints as `float4out` does (`0.4`).

#### Known

- RP-099, what is left: an MCV list over timestamps has no bytes, and a
  timestamp's sub-millisecond part is not in the statistics' values
  (`tests/backlog/test_rp_batch50.py::test_scenario[an MCV list's bytes over timestamps]`).
  A table past ANALYZE's sample has no extended statistics
  (`tests/backlog/test_rp_batch50.py::test_scenario[a table past ANALYZE's sample has extended statistics]`).
