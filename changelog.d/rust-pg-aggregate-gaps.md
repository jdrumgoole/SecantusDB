### The Rust PostgreSQL server: bool_and, bool_or, and honest refusals

Found by sweeping the existing `pg_corpora/` corpora against the Rust server.

`bool_and` and `bool_or` were not recognised as aggregates at all, so they
reached the per-row scalar evaluator. They now work — grouped or not, over a
column or a comparison, skipping NULLs, and answering NULL over an empty input.

`array_agg` over an empty input answered an empty array where PostgreSQL
answers NULL. Every other aggregate over an empty input was already right.

The rest of that cluster is still missing — `string_agg`, and an aggregate
wrapped in an expression such as `count(*) + 1` — but they now refuse while
**planning**. That matters beyond the message: they used to be planned as a
plain SELECT with a computed column, so the refusal came from evaluating a
row, and over an EMPTY table nothing was evaluated, nothing refused, and the
client got zero rows instead of the one row PostgreSQL returns.

#### Fixed

- `crates/secantus-pgplan`: `bool_and` / `bool_or` are aggregates;
  `string_agg` and a nested aggregate are refused while planning.
- `crates/secantus-pgserver`: the two boolean aggregates, and `array_agg` over
  an empty input.

#### Testing

- `tests/test_rust_pgserver_slice.py`: the boolean aggregates, `array_agg`
  over nothing, and that the unsupported shapes refuse the same way whether
  the table is empty or not.
