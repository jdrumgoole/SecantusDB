### `explain` reports the stage tree and the query MongoDB actually parsed

`explain` answered with a single flat node — `COLLSCAN`, or `FETCH` wrapping
`IXSCAN` — and echoed your filter back verbatim as `parsedQuery`. MongoDB does
neither. It wraps the scan in the stages that describe the rest of the query,
and it reports the match expression *after* normalisation. The practical cost of
the flat node was that a client running `explain` to ask "is my sort served by
an index?" could not tell, because the blocking `SORT` stage that answers the
question was never emitted.

Both halves now match. `winningPlan` carries `SORT` (with `sortPattern`,
`memLimit`, and the limit it absorbed), `SKIP`, `LIMIT` and
`PROJECTION_SIMPLE` / `PROJECTION_DEFAULT` in MongoDB's nesting, which is not
the order the command's fields are written in. The `IXSCAN` node carries
`multiKeyPaths`, `isUnique`, `isSparse`, `isPartial` and `indexVersion`;
`COLLSCAN` carries `direction`; `FETCH` carries only the residual filter and
omits the key entirely when the index bounds already cover the predicate, which
is how you tell a fully index-served query from one that re-checks documents.

`parsedQuery` is now the normalised match expression: bare equality grows an
explicit `$eq`, several clauses fold into an `$and` whose children are sorted,
`$ne` becomes `$not`/`$eq`, `$type` becomes numeric BSON codes, `$bitsAllSet`
becomes a bit-position list, and so on. The child order inside `$and` is the
part with nothing documented behind it — it is MongoDB's internal match-type
ordinal, and it disagrees with the enum in MongoDB's own source about where
`$not` sits, so it was derived from ninety-one pairwise probes instead. All
fifty-six filters in the sweep now agree.

What is left out is left out on purpose. `indexBounds`, `rejectedPlans` and the
specialised `IDHACK` / `COUNT_SCAN` / `DISTINCT_SCAN` executors describe
MongoDB's cost model and its plan cache, neither of which this project has ever
claimed to reproduce; and the stage tree is emitted for `find` only, because
`count` and `distinct` use a vocabulary that has not been measured and inventing
one would be worse than the flat node they get today.

#### Added

- `secantus/explain.py`: `canonical_match` and `build_stage_tree`, both pure
  functions over parsed input.
- `tools/probes/explain_shapes.py`: the sweep, including the pairwise derivation
  of the match-type ordering.

#### Fixed

- `commands.py`: `explain` emits the stage tree, the normalised query, the full
  IXSCAN metadata, `COLLSCAN` direction, `isCached` and `explainVersion`.
- `storage.py`: `explain_plan` reports `sorted_by_index`, which is what decides
  whether a blocking `SORT` appears.
