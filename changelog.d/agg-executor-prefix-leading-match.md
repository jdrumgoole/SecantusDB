### A failing `$expr` in a pipeline's first `$match` lost its error prefix

MongoDB wraps an aggregation's runtime failures in `Executor error during
aggregate command on namespace: <ns> :: caused by ::`. SecantusDB did too —
except for the very first stage. A leading `$match` is lifted into the initial
fetch so it can use an index, which put it outside the block that adds the
prefix, so `{$match: {$expr: {$divide: ["$a", 0]}}}` reported a bare
`can't $divide by zero` as the first stage and the full wrapped message
anywhere else in the same pipeline.

`$sqrt`'s domain error also carried a `, but is -1` suffix that MongoDB does not
emit — the one operator in that family that omits it, where `$ln` and `$log10`
keep it.

Of twelve probed runtime-error shapes, eleven now match MongoDB 8.2.11 exactly.
The twelfth is MongoDB re-coding an error raised inside a `$group` accumulator
(`4848401` for a division by zero, `7157706` for `$ln`) — its execution engine
substituting its own code per operator, with no rule derivable short of probing
every accumulator, and deliberately not guessed at.

#### Fixed

- `commands.py`: the lifted leading `$match` gets the executor prefix.
- `expressions.py`: `$sqrt`'s domain message matches MongoDB.
