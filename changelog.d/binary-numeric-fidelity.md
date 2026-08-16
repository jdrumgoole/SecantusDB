### Binary numeric wire fidelity

Two fixes in the binary `numeric` parameter decoder, both pinned by the
pgtest `decimal` corpus file (now fully green). A zero encoded with
thousands of zero digit-groups — CockroachDB's #38139 regression payload
uses 8192 of them — now renders as `0` instead of `0.000…0`: the decoder
quantizes to the declared display scale unconditionally, where previously a
dscale of 0 skipped the quantize and `scaleb` on a zero kept the huge
negative exponent. And a declared scale outside PostgreSQL's
NUMERIC_DSCALE_MASK (for example `0xFFF0`, a negative int16 reinterpreted)
is rejected at Bind with PG's 22P03 `invalid scale in external "numeric"
value`, instead of silently producing an absurd quantization.

#### Fixed
- Binary numeric zeros with non-canonical digit-group padding render as `0`.
- Out-of-range binary numeric dscale raises 22P03 at Bind (was accepted).
