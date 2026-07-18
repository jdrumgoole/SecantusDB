### $facet validates its spec instead of leaking on a malformed sub-pipeline

The `$facet` stage didn't validate its spec. A sub-pipeline element that wasn't a
stage document (`{a: [5]}`) leaked a raw Python `TypeError`, an empty `{}` spec and
a nested `$facet` were silently accepted, and a non-array sub-pipeline gave a
generic error. mongod rejects each: an empty / non-object spec is `40169`, a
non-array sub-pipeline is `40170`, a non-object stage element is `40171`, and a
`$facet` nested inside a `$facet` is `40600`. Both servers now match.

An empty sub-pipeline (`{a: []}`) remains valid. The Python server carries mongod's
codes; the Rust core defers every invalid case (empty spec and nested `$facet`
included) so the Rust server rejects them too. Three-way mongod 7.0.12-verified.

#### Fixed

- `$facet` rejects an empty / non-object spec (`40169`), a non-array sub-pipeline
  (`40170`), a non-object stage element (`40171`), and a nested `$facet` (`40600`),
  instead of leaking a Python `TypeError` or silently accepting the malformed spec
  (both servers).
