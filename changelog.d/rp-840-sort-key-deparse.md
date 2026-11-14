### EXPLAIN prints a computed sort key as its expression

A Sort's key in EXPLAIN now reads as PostgreSQL prints it. An ORDER BY over
an expression used to show the planner's synthetic field (`Sort Key:
__order0`), and a key that repeated a window target showed the target's
output name (`Sort Key: sum DESC`). Both now print the planned expression,
parenthesised as ruleutils does for a subplan output, with window calls as
`OVER (?)`: `Sort Key: ((x + 1))`, `Sort Key: (sum(x) OVER (?)) DESC`.

#### Fixed

- Rust PG server: EXPLAIN of a single-relation SELECT prints a computed
  ORDER BY key (including an ordinal naming a computed target, and a key
  holding a window call) as its deparsed expression.
  Fixes RP-840 (tests/backlog/test_rp_found_rp761.py::test_explain_prints_a_computed_sort_key_as_its_expression)
