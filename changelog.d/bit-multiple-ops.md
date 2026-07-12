### `$bit` update applies multiple operations (both servers)

The `$bit` update operator now accepts more than one bitwise operation per field
and applies them in order, matching mongod: `{$bit: {n: {and: X, or: Y}}}`
computes `(n & X) | Y`. Both servers previously rejected any `$bit` document with
more than a single sub-operation. Found by a three-way update differential vs
real `mongod` 6.0.

#### Fixed

- `update.py` / `secantus-core`: `$bit` iterates every `and`/`or`/`xor` entry in
  the per-field document (in order) instead of requiring exactly one; an empty
  `$bit` document is still rejected, and the int32/int64 result width is preserved
  as before.
