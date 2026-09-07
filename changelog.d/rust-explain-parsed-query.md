### The Rust server's `explain` echoed the filter instead of normalising it

mongod does not echo the filter you sent back in `queryPlanner.parsedQuery` — it
echoes the `MatchExpression` tree **after normalisation**. A bare equality grows
an explicit `$eq`, several top-level fields become an `$and` whose children are
sorted by mongod's internal match-type ordinal, `$ne` becomes `$not`/`$eq`, an
`$in` of one collapses to `$eq`, an `$in` of none becomes `$alwaysFalse`, `$all`
splits into equalities, `$type` becomes numeric BSON codes, a bitmask becomes a
bit-position array, and `$comment` disappears.

The Rust server answered with the filter as sent, so a client reading
`parsedQuery` — the usual way to ask "how did the server understand my query?" —
got its own input back. It diverged from mongod on **44 of the 56 shapes** in
`tools/probes/explain_shapes.py`; the Python server matched all 56, because only
it had `secantus.explain.canonical_match`.

`secantus-core::explain` is now the port of that module, rule for rule, and the
Rust server is at **0 of 56**.

This was found by sweeping the sixteen Rust-aware probes against a server built
from `main`. Twelve of them are completely clean — 740 aggregation-stage shapes,
3,074 operator-error shapes, 409 date/timezone shapes, 244 extended argument
types with no crashes, and more — which is what made the explain result stand
out rather than blend into a general noise level.

Still open, and filed with its measurement: the `winningPlan` plan-node FIELDS.
mongod's `COLLSCAN` carries `direction` and `isCached`, and its `IXSCAN` carries
nine keys where the Rust server emits four. The Python server reproduces those
and sits at its documented floor of 7 of 25 (four `indexBounds`, which this
project deliberately does not reproduce, and three genuine cost-model
differences); the Rust server is at 25 of 25.

#### Fixed

- `secantus-core`: new `explain` module — a port of `secantus.explain`'s
  `canonical_match`, including the match-type rank table, `$type` alias codes
  and the `$nor` decomposition rule.
- `secantus-commands`: `explain`'s `parsedQuery` and the `COLLSCAN` stage
  `filter` report the normalised expression.
