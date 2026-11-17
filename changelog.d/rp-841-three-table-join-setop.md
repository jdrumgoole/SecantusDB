### A set operation over a join of three or more tables is estimated

An INTERSECT or EXCEPT puts the input with fewer estimated groups first, as
PostgreSQL's planner does, and that input's row is the one each group keeps.
A join of three or more tables, every clause a column equality, written with
ON or as a comma join, now has an estimate: the product of its scans' rows
and one `eqjoinsel` per equivalence-class member joined, so `select a.x from
t a, t b, t c where a.k = b.k and b.k = c.k intersect select 1.000` returns
`1.000`, the constant's spelling, as PostgreSQL 15.19 does.

#### Fixed

- A set operation over an inner join of three or more never-analysed tables
  keeps the order PostgreSQL's estimate gives its inputs. RP-841 stays open
  for a HAVING, a DISTINCT and a join with a WHERE.
  Trims RP-841 (tests/backlog/test_rp_found_rp692.py::test_an_intersect_over_a_three_table_join_puts_the_smaller_input_first)
