### The Rust server projects simple field lists without decoding whole documents

Phase 3 of the raw-BSON serving-path work takes the return path off
materialization for the common projection shape. A `find` with a projection
still decoded every returned document into an owned `bson::Document` before
projecting it — the last big materialization site now that projection-free
`find` and `count` run fully on raw BSON.

A pure top-level inclusion projection (`{a: 1, b: 1}`, optionally with
`_id: 0`) now projects straight off the raw document, decoding **only the
included fields** rather than the whole thing. On a 5000-row scan of wide
documents projecting two of twelve fields, that measured **~2× faster**. The
fast path is byte-identical to the full projection — same fields, same order
— so no result changes; anything it doesn't cover (exclusion, dotted paths,
`$slice` / `$elemMatch` / `$meta`, positional, mixed inclusion/exclusion)
transparently falls back to the full projection on a decoded document.

#### Changed

- Rust server: a pure top-level inclusion projection is applied over raw BSON
  (`projection::apply_projection_raw`), decoding only the projected fields
  instead of the whole document. All other projection shapes fall back to the
  full decode + `apply_projection` unchanged.

#### Added

- `secantus_core::projection::apply_projection_raw(&RawDocument, spec)` — the
  raw-BSON inclusion-projection fast path, exposed to the parity harness as
  `_secantus_core.apply_projection_raw` and cross-checked byte-for-byte
  against `apply_projection` on every projection parity case.
