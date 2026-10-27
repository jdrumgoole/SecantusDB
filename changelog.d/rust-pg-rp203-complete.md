### Rust PostgreSQL server: every rule's trees, and what PostgreSQL's analysis refuses

The rule trees `pg_rewrite` holds (RP-203) now cover the constructs the
first version left NULL: `FOR UPDATE` / `FOR SHARE` in a view, a
data-modifying statement in `WITH`, `SEARCH` and `CYCLE`, `ON CONFLICT DO
UPDATE` and `ON CONFLICT ON CONSTRAINT`, `SET (a, b) = (SELECT ...)`,
interval literals with a field qualifier, a `WITH RECURSIVE` name an inner
`WITH` redefines, and a table an `ON SELECT` rule turns into a view. Each is
held to PostgreSQL 15.19's tree, byte for byte after user oids are named.

The same transcription of PostgreSQL's parse analysis now refuses what
PostgreSQL refuses when a view, materialized view or rule is created, with
PostgreSQL's SQLSTATE and message, before anything else is looked at. The
checks are PostgreSQL's general ones, not a list of messages: `DefineView`
and the column checks a new relation goes through, `transformRuleStmt` and
`DefineQueryRewrite` (with the RETURNING and ON SELECT target-list checks),
`checkWellFormedRecursion`, the SEARCH / CYCLE checks, the row-locking
checks, the INSERT row checks and type and collation resolution.

A query grouped by a table's whole primary key may now name the table's
other columns, as PostgreSQL allows (a unique constraint is not enough), and
a view or rule that does so depends on the key: `pg_depend` lists it, and
`DROP CONSTRAINT` is refused while the view stands, or drops it with
`CASCADE`.

#### Fixed

- `pg_rewrite.ev_qual` / `ev_action` for every rule PostgreSQL stores trees
  for. Fixes RP-203
  (`tests/backlog/test_rp_batch51.py::test_scenario[a rule's node trees for
  what the analysis does not transcribe]`, now a regression test, and the
  new scenarios of `tests/test_rust_pg_rule_trees.py`).
- `CREATE VIEW`, `CREATE MATERIALIZED VIEW` and `CREATE RULE` refuse what
  PostgreSQL's parse analysis refuses. Fixes RP-282
  (`tests/backlog/test_rp_top10_found.py::test_rules_and_views_postgres_refuses`
  and `::test_more_of_parse_analysis_refusals`).
- A column functionally dependent on a grouped primary key is accepted, and
  its view's dependency on the key recorded. Fixes RP-280
  (`tests/backlog/test_rp_top10_found.py::test_a_column_functionally_dependent_on_the_grouped_key`,
  `::test_a_unique_key_is_no_functional_dependency`,
  `::test_a_view_leaning_on_the_key_holds_its_constraint`).
- An interval literal with a field qualifier reads a bare number as the
  qualifier's last field (`interval '1' day` was zero), `a:b` as minutes
  and seconds under `minute to second`, and `Y-M` as years and months.
- A table an `ON SELECT` rule turns into a view keeps its relation and row
  type oids, as on PostgreSQL.

#### Known gaps

- A plain `SELECT`'s `WITH RECURSIVE` is planned without PostgreSQL's
  checks: a recursive term of another type, or with an aggregate, is
  answered where PostgreSQL refuses it (RP-295,
  `tests/backlog/test_rp_top10_found.py::test_a_recursive_query_postgres_refuses`).
- Views and rules created before this change keep NULL trees until they are
  replaced: PostgreSQL's trees record source offsets into the original
  statement's query string, which was not kept.
