### Rust PostgreSQL server: `pg_rewrite` holds PostgreSQL's rule trees

`pg_rewrite.ev_qual` and `ev_action` were NULL for every rule: the 142 system
rules `initdb` leaves, every view's `_RETURN` rule and every `CREATE RULE`.
They now hold the node trees PostgreSQL 15.19 stores, byte for byte: the
analysed `Query` of each rule action, printed as `nodeToString` prints it,
with the catalog oids, attribute numbers, type modifiers, collations and
source locations PostgreSQL's carry.

The trees come from a transcription of PostgreSQL's parse analysis
(`analyze.c` and the `parse_*.c` files it calls) and of `outfuncs.c`, run
over `libpg_query`'s raw parse tree and the server's own catalogs. They are
not baked text: a view is analysed when it is created, against the catalog
and `search_path` of that moment, as `DefineView` does. A construct the
transcription does not cover leaves the trees NULL rather than wrong, and the
server logs which construct it was.

#### Fixed

- `pg_rewrite.ev_qual` / `ev_action`: PostgreSQL 15.19's trees for the
  system views' and information_schema's rules (142 of 142 byte for byte),
  for `CREATE VIEW` / `CREATE OR REPLACE VIEW`, `CREATE MATERIALIZED VIEW`
  (kept through `REFRESH` and `ALTER ... RENAME`), and `CREATE RULE`, also
  when PL/pgSQL runs the statement, statically or through `EXECUTE`. Most
  of RP-203 (`tests/backlog/test_rp_batch51.py::test_scenario[a rule's node
  trees]`, now a regression test, and `tests/test_rust_pg_rule_trees.py`);
  what is left is the known gaps below, whose gap test is
  `tests/backlog/test_rp_batch51.py::test_scenario[a rule's node trees for
  what the analysis does not transcribe]`.
- `pg_attribute`: the system catalogs' and views' columns carry their real
  `atttypid` (information_schema's domains) and `attcollation`.
- `pg_type`: array types carry their element type's `typcollation`, and
  `name` and `int2vector` their `typelem`.

#### Added

- `crates/secantus-pgplan/src/nodetostring/`: the analysis and printer:
  SELECT, VALUES and set operations; joins, `USING` / `NATURAL`, LATERAL and
  functions in FROM; WITH and WITH RECURSIVE; sublinks; aggregates (FILTER,
  ORDER BY, DISTINCT, ordered-set and hypothetical-set), window functions and
  frames with ROWS / GROUPS / RANGE offsets; GROUPING SETS, ROLLUP, CUBE and
  `GROUPING()`; function and operator resolution, coercions and casts,
  collations, and the literal input of the built-in scalar types; INSERT
  (with `ON CONFLICT DO NOTHING`), UPDATE (with `SET (a, b) = (...)`),
  DELETE and NOTIFY rule actions. Unit tests check the printer and the datum
  encoders against text PostgreSQL wrote.
- `tools/probes/pg_rule_trees.py`: compares every rule's trees with a private
  PostgreSQL 15.19 cluster: the system rules byte for byte, and 39 scenarios
  of views and rules after replacing user-object oids with names. Its
  `--record` writes `tools/probes/pg_rule_trees.expected.json`, which
  `tests/test_rust_pg_rule_trees.py` holds the server to in CI's `pg-oracle`
  lane.

#### Known gaps

Left NULL, with the reason logged: `FOR UPDATE` / `FOR SHARE` in a view, a
data-modifying statement in WITH, `SEARCH` / `CYCLE`, `ON CONFLICT DO
UPDATE` and `ON CONFLICT ON CONSTRAINT`, `SET (...) = (SELECT ...)`, and an
interval literal with a field qualifier (`INTERVAL '1' DAY`). Views created
before this change keep NULL trees until they are replaced.
