### Rust PG server: a missing column's error suggests the closest column

PostgreSQL answers a reference to a column that does not exist with a 42703
whose HINT names the closest column it could have meant: `select bb from e`
carries `Perhaps you meant to reference the column "e.b".`, two equally
close columns are both named, and a column that exists but is out of reach
(`select * from e, (select b) x`) is named as such. The Rust PostgreSQL
server sent the same 42703 with no HINT.

#### Fixed

- A missing column's 42703 carries the HINT PostgreSQL 15.19's
  `errorMissingColumn` gives it. The search is a port of
  `searchRangeTableForCol` and `updateFuzzyAttrMatchState`: a Levenshtein
  distance over every range table entry of the reference's query level and
  each level enclosing it, the qualifier's distance to an entry's alias
  added, more than half the name different rejected, two ties named and
  three named none. Each level's range table is rebuilt from the statement
  as written, in the order parse analysis adds its entries: tables and views,
  CTEs with their column aliases, subqueries and VALUES lists by their output
  names, a set operation's earlier arms as `*SELECT* n`, an UPDATE's or
  DELETE's target before its FROM / USING, an INSERT's target above its
  query, and ON CONFLICT DO UPDATE's `excluded`. It runs over the simple and
  the extended protocol. Fixes RP-408
  (`tests/backlog/test_rp_column_hint.py::test_missing_column_hint`).

#### Known gaps

- A function in FROM has the columns of its result type, which the search
  does not know, so a statement with one gets no HINT (RP-413).
- Filed while measuring: a 42703 that names the wrong reference or drops its
  qualifier (RP-414), a column in an INSERT's VALUES refused as `0A000`
  (RP-415), and a join's ON after a comma item not type-checked (RP-416).
