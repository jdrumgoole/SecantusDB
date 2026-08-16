### Multidimensional arrays keep their base type; JSON[] keeps oid 199

Two array-typing fixes from the pgtest corpus. A nested array
constructor (`ARRAY[ARRAY[1], ARRAY[2]]`) typed as `text[]` — its
binary wire form carried text elements where PG uses ONE array oid per
element type regardless of dimensionality, so clients read integer
arrays as strings. The tag inference now recurses into nested
constructors, and the multidimensional binary encoding carries the
element type's oid and binary cells.

And `::JSON[]` now keeps the plain-json array identity end-to-end —
parameter descriptions and row descriptions report oid 199 (not
jsonb-array's 3807), and the binary array header carries element oid
114 — extending the earlier scalar `::json` → 114 rule to arrays. The
one remaining `json_array` corpus divergence (a hand-spaced json
element re-rendering compact where PG echoes the client's text
verbatim) is the documented parsed-storage tradeoff, now recorded as an
expected divergence in the gauge.

#### Fixed

- `sql/planner.py`: nested array constructors type as their base array
  type; `$1::JSON` / `$1::JSON[]` parameter inference keeps the
  plain-json oids.
- `sql/typemap.py`: `cast_type_identity` reports 199 for `::JSON[]`.
- `pgtest_validation/include_paths.py`: `json_array` recorded as an
  expected divergence (verbatim-json, deliberate).
