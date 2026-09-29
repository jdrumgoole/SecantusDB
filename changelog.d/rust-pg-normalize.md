### `normalize()` on the Rust PostgreSQL server

The one string function left after the last release needed something the others
did not: a dependency. Unicode normalisation is table-driven — the composition
and decomposition mappings are data rather than an algorithm — so there is no
honest way to approximate it in a few lines. An ASCII-passthrough version would
have answered most inputs correctly and a minority silently wrongly, which is
exactly the kind of divergence this project treats as unacceptable, so it stayed
refused by name until the dependency was agreed.

It is implemented now, on `unicode-normalization`, and all four forms match
PostgreSQL 14.13: `NFC` (the default), `NFD`, `NFKC` and `NFKD`. A precomposed
`á` decomposes to two codepoints under NFD and recomposes to one under NFC; the
compatibility forms fold the `ﬁ` ligature to `fi` where the canonical ones leave
it alone.

#### Added

- `normalize(text [, form])`. The form arrives as an ordinary string constant —
  `NFD` and its siblings are grammar keywords, so an unknown one is a syntax
  error before the evaluator ever sees it.

#### Known limitations

- `IS NORMALIZED` and `IS NORMALIZED NFD` are a separate construct and remain
  unimplemented.
