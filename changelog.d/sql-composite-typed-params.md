### SQL server: composite records end to end, typed array parameters, NaN semantics

The composite/record machinery closes its remaining conformance gaps.
Array-of-composite elements binary-encode as real records (not JSON), a
binary composite parameter with a NESTED composite field recurses by the
field's minted OID instead of crashing, a registered dumper's record text
literal (`'("(foo,10)",20)'::"-x-€"`) parses on INSERT — including nested
and zero-field (`'()'`) types — and a declared-composite table column
reports its minted OID so a registered psycopg loader parses nested fields
by their reflected types. Anonymous `row(…)` records now carry each field's
SQL type OID from the source expression: an untyped literal embeds
unknown (705, loads as bytes like real PG), `::text` embeds 25, `::bytea`
17 with real binary bytea, and int literals type int4. `VALUES` rows of
records describe as RECORD, and `array_agg` types as the element's real
array type (jsonb_agg/json_agg keep json) — psycopg's
`CompositeInfo.fetch` of a zero-field type depends on it. Parse, Bind, and
Describe now run inside the open transaction's storage scope, so a type
created earlier in an uncommitted block is visible to parameter-OID
resolution.

Typed array parameters generalize: a text- or binary-format array param
with a known array OID (int2[]/numeric[]/inet[]/bytea[]/…) decodes into a
typed list and substitutes through a `::tag[]` cast, so equality against
`array[…]` constructor values compares element-wise (this closed the numpy,
uuid, and network dump/load clusters wholesale). Array casts coerce LIST
values to the element's canonical form, bare `array[x::inet, …]`
constructors describe as the element's array type, uuid/inet/cidr/macaddr
text parameters canonicalise at Bind (psycopg dumps uuids as bare hex), and
`NaN = NaN` is true like Postgres. psycopg's composite, numpy, uuid, net,
and numeric suites now pass (numeric's exhaustive wide-digit test remains —
the Decimal128 34-digit cap).
