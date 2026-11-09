### Rust PG server: an array column sorts by `array_cmp`, an array whose lower bound is not 1 included

An ORDER BY over an array column orders the arrays the way PostgreSQL's
`array_cmp` does. That covers a GROUP BY's ORDER BY, a DISTINCT's, a window's,
`min` / `max` and a plain sort, ASC or DESC. The elements are compared first,
in row-major order whatever the shapes. Then the array with fewer elements
comes first, then the one with fewer dimensions, then the one with shorter
dimensions, and last the lower bounds. An array whose lower bound is not 1
(`'[0:1]={x,y}'`) had no order beside a plain array. Every such pair compared
equal, and a column holding both sorted out of order, the same way ASC and
DESC. Two-dimensional arrays were compared row by row instead of element by
element.

#### Fixed

- `select count(*), a from x719 group by a order by 2` over `'{ab}'`,
  `'{cd,ab}'` and `'[0:1]={x,y}'` returns `{ab}`, `{cd,ab}`, `{x,y}`, as on
  PostgreSQL 15.19 (was `{cd,ab}`, `{x,y}`, `{ab}`). `order by a desc`
  reverses it.
- `'[0:0]={ab}' < '{ab}' < '[2:2]={ab}'` in a sort, and
  `'{{1,2,3}}' < '{{1,2},{3,4}}'` (the second has one more element).

Fixes RP-722 (tests/backlog/test_rp_found_rp719.py::test_groups_of_an_array_column_follow_the_order_by)
