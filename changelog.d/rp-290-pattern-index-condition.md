### psql's name pattern counts the catalog reads PostgreSQL makes for it

When psql describes a table it finds it with a regular expression on `pg_class.relname`, `'^(name)$' COLLATE pg_catalog.default`, filtered by `pg_table_is_visible`. PostgreSQL turns a pattern whose fixed prefix is the whole of it into an index condition: the operator's support function checks that the collation is deterministic, finds the equality `name = text` in the index's operator family, and the B-tree scan then looks up that cross-type comparison function. The `COLLATE` clause looks up its argument's type and the collation by name, encoding first. `pg_table_is_visible` reads each row it is called on by oid, then searches for the relation's name in every schema ahead of its own on the search path. The cumulative statistics (`pg_stat_sys_tables`, `pg_stat_sys_indexes`) now count those reads as PostgreSQL 15.19 does: one `pg_class` index scan where this server counted a sequential scan of the whole catalog, plus each lookup above. On psql's own `\d rpc_t` query, 5 counters now differ where 10 did; what remains is the join with `pg_namespace`.

The same reads now follow a LIKE without a wildcard, an explicit `name = text`, and a regular expression on a `name` column without `COLLATE`. The last also counts PostgreSQL's failed attempt to prove the pattern from the index condition, which reads the operators' B-tree interpretations.

#### Fixed

- Catalog-read counts for a regular expression or LIKE whose fixed prefix is the whole pattern: the index condition it becomes, the collation's determinism check, the equality's membership and estimator, `_bt_first`'s cross-type comparison proc, and the predicate proof against the condition (RP-290, part).
- Catalog-read counts for `COLLATE` (`type_is_collatable`, `lookup_collation`) and for `pg_table_is_visible(c.oid)` restricting a scan of `pg_class` (RP-290, part).
- A statement with a pattern operator is no longer taken to read what an earlier statement with other literals read: the pattern decides what the planner reads.

#### Added

- Backlog item RP-934 (an ORDER BY an index scan already returns in order is counted as sorted, and the index's pathkeys are not read), with a gap test.

Fixes RP-290, in part (tests/backlog/test_rp_catalog_reads.py::test_gap[a pattern's exact match is an index condition], tests/backlog/test_rp_catalog_reads.py::test_gap[psql's pattern on one relation])
