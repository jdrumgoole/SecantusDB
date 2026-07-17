### SQL server: enum OIDs through every plan shape, and enum-array columns

The minted enum OID now survives every SELECT plan shape. GROUP BY keys,
JOIN projections, `SELECT DISTINCT`, and per-row-evaluated selects (a scalar
function alongside the enum column) previously described enum result columns
as plain `text` (25) because the pipeline/evaluated planners flatten output
columns to string type tags; the enum identity now travels in a parallel
`out_enum_types` position map so RowDescription reports the mint — and a
psycopg `register_enum` loader fires on those results — in both the simple
and extended protocols. `array['sad'::mood, …]` constructors describe with
the minted array-companion OID like the `::mood[]` cast already did.

`mood[]` table columns land too: an array of a declared enum type was
previously rejected outright (`unsupported column type`); it now stores a
text array, validates every element against the enum's labels at write time
(22P02), and reports the array-companion OID so a registered loader returns
lists of enum members. An array of an undeclared type raises 42704.

#### Added

- `planner.py`: `out_enum_types` on `PipelineSelectPlan` / `EvaluatedSelectPlan`,
  populated by the DISTINCT / GROUP BY / JOIN / evaluated builders;
  `_enum_array_element_name` recognises `mood[]` column declarations; the
  constant-select array-constructor override gains an enum branch.
- `executor.py`: `_tagged_out_column_descs` resolves minted enum OIDs for the
  string-tag plans (shared by Execute and Describe); `_out_column_descs`
  reports the array-companion OID for enum-array columns; enum write
  validation checks each element of an array value.
