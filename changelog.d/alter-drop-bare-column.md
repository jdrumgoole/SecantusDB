### ALTER TABLE … DROP without the COLUMN keyword

`ALTER TABLE t DROP name` — the keywordless column-drop form real
Postgres accepts — no longer errors. sqlglot parses the action as a raw
Command; the executor now recognises `DROP [IF EXISTS] <col>
[CASCADE|RESTRICT]` there and applies the standard drop-column action.
(pgjdbc's DatabaseMetaDataTest droppedColumns.)

#### Fixed
- Keywordless `ALTER TABLE … DROP <col>` (quoted or bare, IF EXISTS,
  CASCADE/RESTRICT tails).
