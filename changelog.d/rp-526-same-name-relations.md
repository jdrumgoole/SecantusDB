### Two schemas' tables of one name can share a FROM clause

The Rust PostgreSQL server now follows PostgreSQL's namespace rule for two
unaliased FROM items of the same name: when they are different relations
(`s1.t` and `s2.t`) the query runs, `s1.t.a` and `s2.t.a` each read their own
table, a bare `t.a` is 42P09 `table reference "t" is ambiguous`, and a bare
`a` is 42702. Before, every join of the two was refused with 42712, and the
comma form was worse: `select s2.t.a from s1.t, s2.t` silently read `s1.t`'s
column. A view whose FROM gives one item an alias equal to another item's
name is now refused with 42712 at CREATE VIEW, as PostgreSQL refuses it.

#### Fixed

- Unaliased FROM items of one name and different relations are allowed, keyed
  by their relation; `schema.t.col` resolves to the one that is that relation.
- `t.col` naming two such items is 42P09.
- CREATE VIEW checks its FROM for repeated names (42712).

Fixes RP-526 (tests/backlog/test_rp_found_rp467.py::test_same_name_relations_in_from)
