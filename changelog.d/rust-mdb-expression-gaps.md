### Rust MongoDB server: date arithmetic, binData conversions and expression errors match mongod

Date arithmetic in `$add` / `$subtract` and the missing expression operators
now behave as mongod 8.2.11 does. Each case was measured against mongod.

#### Fixed

- `$add` and `$subtract` with a Date work in aggregation, `$expr` and
  pipeline updates. A double operand rounds half away from zero and a
  decimal half to even, and `$add: []` is 0. NaN or overflow is 15, two
  dates added is 16612, and a number minus a date is 14. Fixes RM-212
  (`tests/backlog/test_rm_found_2026_10_20b.py::test_date_arithmetic`).
- `$unsetField` and `$toUUID` are implemented.
- `$convert` reads and writes binData:
  - string to binData and back, in every `format`;
  - numbers to binData, with `byteOrder`;
  - the subtype rules, and every refusal under its own code;
  - binData read as a number is signed (`BinData(0, "FF")` is -1, not 255);
  - `$toString` of a 16-byte UUID gives the UUID form.

  Fixes RM-213 (`tests/backlog/test_rm_found_2026_10_20b.py::test_expression_operators`).
- Refusals that answered a generic 2, or a wrong value, now carry mongod's
  codes and texts:
  - `$substrCP` and `$substrBytes` indexes;
  - `$setField` / `$unsetField` input and field;
  - `$zip`, `$map` and `$filter` inputs;
  - `$slice` with a non-numeric position or count;
  - `$let`, `$map` and `$filter` variable names;
  - the `$arrayToObject` element rules;
  - a dotted key in an object literal (16412);
  - `$dateToString` of a Timestamp or ObjectId.
- `$getField` on a non-object leaves the field missing, as mongod does.
