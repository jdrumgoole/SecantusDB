### Rust-server residues closed: $project error fidelity, WT knobs, full $min/$max order

Three long-tracked Rust-server gaps from the rewrite backlog are closed. An
unknown expression operator inside an aggregation `$project` now reports
mongod's stage-specific `Location31325` on the Rust server too (a parse-time
scan that never mislabels the projection-only `$slice` / `$elemMatch` /
`$meta` shapes), completing the context-specific unknown-operator codes on
both servers. The embedded `RustServer` handle grew the WiredTiger knobs the
daemons already exposed — `cache_size`, `session_max`, and `sync_on_commit`
constructor parameters — so tests can drive non-default storage configs
in-process. And the Rust engine's `$min`/`$max` update operators moved onto a
direct port of Python's `_bson_lt` (`order::bson_lt`), a single strict-less
relation that needs none of the `$sort` comparator's transitivity guarantees:
bool, Decimal128, NaN, Binary, Timestamp, Regex, Min/MaxKey, and the decoded
exotic text types all compute natively now, with only a DBPointer operand
still deferring. Range operators accept the exotic text types the same way
(Symbol / JS code compare as strings, mirroring pymongo's decode), so
otherwise-fine queries no longer error with `BadValue` on the Rust server.

#### Added

- `RustServer(..., cache_size=, session_max=, sync_on_commit=)` — the
  embedded handle's WiredTiger knobs, threading into `wt_config` exactly like
  `secantusd-rs`'s `--cache-size` / `--session-max` / `--sync-on-commit`.
- `order::bson_lt` in `secantus-core` — the direct `ordering._bson_lt` port
  backing `$min`/`$max`, covering the types `is_sortable` must bar from sorts.

#### Fixed

- Rust server: an unknown expression operator inside `$project` reports
  `Location31325` ("Invalid $project :: caused by :: Unknown expression $op")
  instead of a generic `2 BadValue`, matching the Python server and mongod.
- Rust matcher: JS code / Symbol range operands compare as text and DBPointer
  / undefined operands are a clean no-match, instead of erroring `BadValue`.
- Rust engine: `$min`/`$max` with bool / Decimal128 / NaN / Binary /
  Timestamp / Regex operands compute instead of deferring; curated parity
  cases pin every newly-computed shape.
- `invoke rust-parity` installs `shapely` / `s2sphere` / `python-dateutil`
  into its isolated environment, so the geo curated cases run instead of
  erroring with `ModuleNotFoundError`.
