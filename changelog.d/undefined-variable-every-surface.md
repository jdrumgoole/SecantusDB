### An undefined `$$variable` answers 17276 on every surface, not just aggregation

The parse-time check for an undefined `$$variable` reached the aggregation
pipeline. Every **other** surface that takes a filter or an update still fell
through to the storage layer's generic `BadValue` (2) — `query uses a construct
the Rust server does not support` — on the Rust server, and the Python server
failed whole write batches that mongod fails one statement of.

Measured on mongod 8.2.11: **the Rust server was wrong on all 8 surfaces, the
Python server on 4.**

#### Fixed

- **`find`, `count`, `distinct` and `findAndModify`** now answer 17276 for an
  undefined variable inside a filter's `$expr`, instead of the generic
  unsupported-construct error.
- **`update` and `delete` report it per STATEMENT**, in `writeErrors` with code
  17276 — not as a command error. This matters beyond the code: mongod applies
  the earlier statements in the batch and fails only the offending one
  (`n: 1` with the error at `index: 1`), where the Python server failed the
  whole batch and the Rust server reported code 2.
- **A pipeline-form update carries the stage wrapper.** `{"u": [{"$set":
  {"b": "$$NOPE"}}]}` is `Invalid $set :: caused by :: Use of undefined
  variable: NOPE`; a filter never takes a wrapper.

#### How it is checked

A filter is **query language**, not an expression: `{s: "$$NOPE"}` matches the
literal string and must not be flagged. Only `$expr` holds an expression, and
only `$and` / `$or` / `$nor` nest further filters — so the new filter walker
descends into exactly those and leaves everything else alone, the same
conservative rule the pipeline walker follows. A false positive would reject a
valid query, which is worse than the wrong code being fixed.

Command-level `let` binds, and is threaded through on every surface.

14 cases added to `tests/test_mongod_differential.py`, four of them
false-positive guards (a literal `"$$NOPE"` in a filter, a bound `let`, and two
ordinary queries), plus the per-statement case that pins `n: 1`.
