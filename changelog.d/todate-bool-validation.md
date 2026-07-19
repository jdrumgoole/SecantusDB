### $toDate rejects a bool instead of coercing it to a date

`$toDate` (and `$convert` to `date`) silently coerced a bool to a date by treating
it as `1` / `0` milliseconds. mongod rejects it: a bool is a `ConversionFailure`
(241, "Unsupported conversion from bool to date"), which `$convert`'s `onError`
still catches. Every other supported source (int / long / double / string /
objectId / decimal → date, null → null) is unchanged. Both servers now match.

The Python server carries mongod's 241 code (through `$toDate`, which previously
re-wrapped it as a generic error); the Rust core now classifies bool → date as a
supported-but-failed conversion so `$convert`'s `onError` applies on the Rust
server too. Three-way mongod 7.0.12-verified.

#### Fixed

- `$toDate` / `$convert` to `date` reject a bool with `ConversionFailure` (241)
  instead of coercing it to a date, and `$convert`'s `onError` handles the failure
  (both servers).
