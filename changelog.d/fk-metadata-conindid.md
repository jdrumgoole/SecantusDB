### Foreign-key metadata: conindid, referential-action codes, and a 3x faster $unwind

pgjdbc's getImportedKeys/getExportedKeys/getCrossReference returned
zero rows: a foreign key's `pg_constraint.conindid` was 0, and the
metadata query joins `pkic.oid = con.conindid` to name the referenced
PK index — the join silently emptied every FK result. FK rows now point
conindid at the referenced table's PK index and carry the one-letter
`confupdtype` / `confdeltype` referential-action codes (CASCADE 'c',
SET NULL 'n', SET DEFAULT 'd', RESTRICT 'r', NO ACTION 'a').

The same query exposed an aggregation hot spot: `$unwind` deepcopied
every fanned-out doc, dominating high-fanout join pipelines (the FK
query is a 9-way join). When no stage in the pipeline (including nested
$lookup/$facet/$unionWith sub-pipelines) mutates docs in place, unwind
now fans out with shallow top-level copies — every writing stage
deepcopies its input first, so shared subtrees are never corrupted.
~3x on the FK metadata query; benefits MongoDB-side aggregations with
the same shape.

#### Fixed
- `pg_constraint.conindid` on FK rows (was 0); new `confupdtype` /
  `confdeltype` columns.
- `$unwind` shallow fast path gated on pipeline mutation analysis
  ($fill / $densify anywhere in the pipeline keep the deepcopy path).
