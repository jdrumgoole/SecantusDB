### An INTERSECT over a GROUP BY expression or a grouping-column HAVING puts the smaller input first

A set operation hashes its input with fewer estimated groups first, and each
group's row is the one its first input gave. An aggregated input grouped by an
expression (`group by x * 1`), or filtered by a HAVING over a grouping column
(`having x > 0`), was not estimated, so the left input stayed first and the
row came from the wrong side. The server now estimates the groups as
PostgreSQL does: an expression key counts the columns it reads, and a HAVING
term over a grouping column moves to the WHERE before the groups are counted.

#### Fixed

- `select x * 1 from t group by x * 1 intersect select 1.000` and `select x
  from t group by x having x > 0 intersect select 1.000` return `1.000`, as
  on PostgreSQL 15.19.

Fixes RP-841 (tests/backlog/test_rp_found_rp692.py::test_an_intersect_over_another_unestimated_input_puts_the_smaller_input_first)
