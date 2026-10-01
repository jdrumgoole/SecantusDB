### Rust PostgreSQL server: aggregate FILTER with a subquery

`count(*) FILTER (WHERE EXISTS (SELECT ...))`, and any other `FILTER` condition holding a subquery, used to answer `0A000 SubLink is not supported yet`.

#### Fixed

- An aggregate that skips NULL inputs now accepts a subquery in its `FILTER`, correlated or not. This covers `count`, `sum`, `avg`, `min`, `max`, `string_agg`, the `bool_*` and `bit_*` aggregates, and the variance family. The filter is rewritten to the `CASE` argument it is equivalent to.
- It works in the select list, `HAVING` and `ORDER BY`, and with `GROUP BY` and `DISTINCT`. Corpus `filter_sublink` matches PostgreSQL 14 on all 13 lines.
- NULL-keeping aggregates such as `array_agg` still refuse with `0A000`.
