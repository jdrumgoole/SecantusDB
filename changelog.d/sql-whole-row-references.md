### A table alias can stand for the whole row

#### Fixed

- `row_to_json(t) FROM (SELECT ...) t` — one of the commonest ways to get a row
  out as JSON — answered `42703 column "t" does not exist`. So did
  `to_json(r)`, `row_to_json(<table>)`, `(<table>)::text` and
  `SELECT t FROM t`. A table or sub-select alias now stands for the whole row,
  as it does in PostgreSQL.

A real column of the same name still wins, which is what PostgreSQL does.

One gap remains: `SELECT t FROM t` reports the generic `RECORD` oid where
PostgreSQL reports the table's own rowtype oid. The field values are correct;
minting per-table rowtype oids is a catalog feature, recorded in
`tasks/backlog.md`.
