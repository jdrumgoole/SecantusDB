### A computed ORDER BY key through a table alias prints its expression

`explain select a from k x order by x.a*2` printed the Sort's key by the
server's internal field name, `Sort Key: __order0`, because the key's
`x.a` did not resolve against the alias. The key is now deparsed through
the alias's range-table entry, `Sort Key: ((a * 2))`, as PostgreSQL 15
prints it, and as the same query without an alias already did.

#### Fixed

- Rust PG server: a computed ORDER BY key qualified by the FROM's alias
  prints its expression in EXPLAIN's `Sort Key`.
  Fixes RP-1035 (tests/backlog/test_rp_explain_costs.py::test_plan[a computed sort key through an alias prints its expression]; crates/secantus-pgplan/src/ruleutils.rs::tests::a_sort_key_through_an_alias_is_deparsed)
