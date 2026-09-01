### The Rust server answers mongod's argument errors

`tools/probes/operator_error_surface.py` crosses every query and update operator
with every pathological argument. Against the Rust server it reported **1,053
divergent shapes, 999 of which answered `BadValue: "query uses a construct the
Rust server does not support"`** for an argument mongod names precisely —
`Unknown type name alias: x`, `Expected a number in: n: "x"`, and so on. It is
now **17**, on a corpus that grew 38% during the work.

840 of those 999 had the right *code* by accident, because mongod's parse errors
are `BadValue` and so is the generic refusal. A code-only comparison had made
this look like 159 problems.

#### Fixed

- Every query operator names its own argument errors: `$mod`, `$size`, `$type`,
  `$in` / `$nin`, `$all`, `$elemMatch`, `$regex`, `$not`, and
  `$and` / `$or` / `$nor`.
- Every update operator likewise: `$pop`, `$rename`, `$bit`, `$currentDate`,
  `$push`, `$pull`, `$pullAll`, `$addToSet`.
- **The storage layer's update path threw the named error away** with
  `map_err(|_| QueryUnsupported)` — the same erasure the query path had before
  it gained `query_fault`. That one line was why the engine's messages could not
  reach a client.
- `$exists` read every `Decimal128` as truthy, so `{v: {$exists:
  Decimal128("0")}}` matched where mongod matches nothing. **This was wrong on
  both servers**, and the sweep missed it until the corpus gained a zero
  `Decimal128` — a value that is falsy in one BSON type and truthy in another is
  exactly what such a corpus needs.
- `{v: {$type: NaN}}` crashed the Rust server with `internal server error`.
- `Decimal128("-0")` was rejected as unrepresentable everywhere it appeared,
  because whole-ness was tested by comparing parsed forms structurally and `-0`
  is not structurally `0`.
- `bson_type_name` reported `object` for MinKey, MaxKey, Timestamp, Undefined,
  Symbol, DbPointer and JavaScript — everything its match arms did not name.
