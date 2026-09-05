### A `$set` of `-0.0` over `0.0` was silently not stored

`update_one({...}, {"$set": {"a": -0.0}})` on a stored `0.0` did nothing:
`modifiedCount: 0`, no change-stream event, and a read-back of `0.0`. The value
the caller asked to store was never stored. mongod stores it and reports the
update (probed 8.2.11, 2026-09-05).

`storage.py` guarded the write with `if new != doc`, and Python's `==` treats
`-0.0` and `0.0` as equal — and an `int` `0` as equal to a `float` `-0.0`, so a
numeric **type** change was dropped the same way.

**Fixing it un-masked a second bug that had been cancelling it.** Our `$mul`
computed `-0.0` for `0.0 * -1` where mongod keeps `0.0`; the wrong product was
previously discarded by the very comparison being fixed. mongod's rule, measured
across 15 shapes: a stored double or decimal zero keeps its own sign whatever
the multiplier, while a non-zero result (`0.0 * inf` is NaN) writes normally and
an `int` zero promotes and follows IEEE.

**The change-stream diff was blind the same way**, in two places: a `$set` of
`-0.0` over `0.0` produced an update event with an EMPTY `updatedFields` — the
consumer was told the document changed and never told which field — and so did
a numeric type change.

**Equality and change detection are different questions**, which is why this is
not a one-line fix to the shared helper. mongod calls `0.0` and `-0.0` the same
value for `$eq` (`$cmp` is 0, `find({a: -0.0})` matches a stored `0.0`) and
different for change detection. Folding the rule into `bson_equal` /
`expressions::py_eq` would have broken `$eq` and query matching; both engines
get a separate change-detection predicate instead.

A Rust test had **canonised the bug as the specification**:
`numeric_bridge_no_change` asserted that `{a: 1}` -> `{a: 1.0}` emits nothing,
justified by the comment `1 == 1.0 -> no update emitted` — Python's equality
rule cited as though it were the server's. It is replaced by tests asserting the
measured behaviour, plus a guard that an unchanged value still emits nothing.

Remaining and deliberately untouched: `$set` of a NaN over the same NaN emits a
spurious event here where mongod emits none. That is the per-OPERATOR
`nModified` rule (mongod's `$set` skips a byte-identical NaN while `$inc: 1` on
that same NaN writes, the stored bit pattern `000000000000f87f` either way),
already tracked in `tasks/backlog.md`.

#### Fixed

- `storage.py`: the write guard counts a difference in the ENCODED BSON as a
  change, so signed zero and numeric type changes are stored.
- `update.py`, `crates/secantus-core/src/update.rs`: `$mul` leaves a stored
  double / decimal zero's sign alone.
- `ordering.py` (`bson_same_stored_value`), `diff.py`,
  `crates/secantus-core/src/diff.rs` (`same_stored_value`): change detection
  distinguishes signed zero and numeric type, recursing into arrays and
  subdocuments so the array fast path sees them too.
