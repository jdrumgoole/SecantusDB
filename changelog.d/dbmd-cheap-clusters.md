### DatabaseMetaData catalog gaps: max_index_keys, proargmodes, reltuples, output-alias ORDER BY

Four of DatabaseMetaDataTest's failure clusters, all catalog or planner
gaps. `pg_settings` now carries `max_index_keys` (32 — pgjdbc reads it
once per connection and every foreign-key metadata call died without
it). `pg_proc` gains `proargmodes` / `proallargtypes` (NULL — no OUT
params recorded) so getFunctionColumns runs. `pg_class` gains
`reltuples` (−1, PG's "no estimate yet") so getIndexInfo's CARDINALITY
reads. And ORDER BY naming a computed output alias in a join query —
pgjdbc's getTables sorts by the CASE-computed `"TABLE_TYPE"` — now
resolves through the select list like the single-table and grouped
paths already did, instead of raising 42703.

#### Added
- `pg_settings.max_index_keys` (32), `pg_proc.proargmodes` /
  `proallargtypes`, `pg_class.reltuples` (−1).

#### Fixed
- Evaluated-join ORDER BY resolves computed output aliases and ordinals
  (`ORDER BY "TABLE_TYPE"`, `ORDER BY 1`) instead of erroring 42703.
