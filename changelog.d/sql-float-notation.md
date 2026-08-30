### Floats picked the wrong notation, on both float types

`SELECT CAST(80 AS REAL)` put `8e+01` on the wire where PostgreSQL puts `80`.
The value was never wrong — only its spelling — which is exactly why it survived
so long: a driver decodes `80` and `8e+01` to the same Python float, so a
value-level comparison cannot see it at all. It took registering a raw text
loader and comparing the bytes.

Underneath was one confusion in two places. Emitting a float is *two* decisions —
how many significant digits round-trip, and whether to print fixed or scientific
— and both renderers derived the second from the first. PostgreSQL derives it
from the **exponent alone**: scientific iff `exp < -4` or `exp >= 6` for
`float4`, `>= 15` for `float8`.

#### Fixed

- **`float4` went scientific far too early.** The renderer searched for the
  shortest round-trip with `f"{value:.{p}g}"` and returned that string, but `%g`
  switches to exponent form whenever `exp >= precision` — so 80, which
  round-trips on a single digit, printed as `8e+01`. Values needing more digits
  (`3`, `64`, `123.456`) were unaffected, which is why this looked value-specific
  rather than systematic.
- **`float8` went scientific too late.** It used Python's `repr`, whose
  threshold is 16 where PostgreSQL's is 15, so `1e15` printed as
  `1000000000000000` and `9007199254740992` as itself, where PG gives `1e+15`
  and `9.007199254740992e+15`. Found by sweeping `float8` after fixing `float4`,
  on the suspicion that the same conflation lived there too.

Both now share `typemap._shortest_round_trip`, so the digit search and the
notation rule cannot drift apart again. Verified against a live PostgreSQL 14
over 637 `float4` and 620 `float8` values — 400 of each random bit patterns
across the whole range — with zero mismatches.

This also closes a sqllogictest gauge failure: `random/select/slt_good_1.test`
compares raw text and now passes, taking that lane back to the committed 52/60.

#### Also

Two backlog corrections, both found by re-probing rather than reading:
`_pg_expandarray(...).x` was still filed as returning TEXT but was fixed on
2026-08-29 (value *and* OID match PG for int, text and numeric arrays), and
`pg_typeof()` over a set-returning function returns one row where PostgreSQL
returns N — pre-existing, now recorded with its cause.
