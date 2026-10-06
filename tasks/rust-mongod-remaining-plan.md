# Rust MongoDB server: the remaining features -- plan (2026-10-06)

Scope: what is still open for `secantusd-rs` against mongod 8.2.11, taken from
`tasks/backlog.md` sections 7.00-7.03 and the gauge entries, ordered by what a
driver hits on the wire. The exemplar is mongod only; the Python server is never
the reference (CLAUDE.md, "Design constraints").

**Where things stand, measured 2026-10-06:**

| Surface | Divergent against mongod |
| --- | --- |
| Date strings, with and without `format` (`date_string_parsing.py`) | 0 of 433 |
| Aggregation expressions (full sweep) | 7 of 6,628, all the authorised last-digit decimal transcendentals |
| pymongo gauge | 1,205 / 5, only declared non-goals left |

## Phase 0 -- re-measure before building: DONE (2026-10-06)

Run against `secantusd-rs 0.5.3-beta.170` (tree `4d2948c6`) and mongod 8.2.11,
a replica set where write concern or change streams are involved.

**The headline: most of the list this plan was written from does not apply to
the Rust server.** The §7.00 entries it was built from sit under the backlog's
heading "Found and NOT fixed -- Python-server divergences". They were read as
Rust items when this plan was drafted. Re-measured on the Rust binary:

| Item | Rust vs mongod |
| --- | --- |
| awaitable `hello` (`awaitable_hello.py`) | 2 of 33 -- one error code |
| `maxTimeMS` expiry (`max_time_expiry.py`) | 3 of 11 -- message prefix |
| regex `\Z` / `\z` (`regex_value_semantics.py`) | 0 (Python: 2) |
| change streams (`change_streams.py`, `change_stream_fuzz.py`) | 0 of 41, 0 of 60 |
| `remaining_shapes.py` (new: `$jsonSchema`, write concern, `$project`, positional update, §7.00 expressions) | 6 of 38, one authorised |

Clean on the Rust server: regex anchors, `$project: {_id: 1}`, negative
`$slice`, `$bucketAuto` over decimals, decimal `$pow`, the positional update,
write-concern `errInfo` and an unknown `w` tag. The detail is in backlog §7.00
("Rust server, re-measured 2026-10-06").

## Phase 1 -- the Rust divergences phase 0 found (about a day)

All on the wire, all small:

1. **`$jsonSchema` `type: "integer"`** -- refuse with 9 `$jsonSchema type
   'integer' is not currently supported.`, at the top level and in
   `properties`, on `create` / `collMod` / `find`.
2. **A schema failure on update** -- send mongod's
   `Plan executor error during update :: caused by :: Document failed
   validation` with the `errInfo` tree. Insert already builds that tree; this
   is wiring the same builder into the update path. Check `findAndModify` and
   a replacement update too.
3. **`maxTimeMS` on writes** -- prefix `Plan executor error during
   findAndModify / update / delete :: caused by ::`.
4. **Awaitable `hello`** -- a newer `topologyVersion` counter is 51764, not
   31382.
5. **`writeConcern.w` of the wrong type** -- 9 `w has to be a number, string,
   or object; found: <type>`. Probe the other bad types (bool, double, null)
   while there.
6. **Decimal `$log` with a base** -- answer it. Use the same correctly rounded
   decimal path as `$ln`, if it exists, or decline with a reason.

**Exit:** `remaining_shapes.py` at 1 of 38 (the authorised `$sin` digit),
`max_time_expiry.py` 0 of 11, `awaitable_hello.py` 0 of 33.

## Phase 2 -- folded into Phase 1

`$jsonSchema` `errInfo` turned out to be an update-path wiring gap, not the
missing tree builder this plan assumed. The regex, `$project` and positional
items were Python-server entries.

## Phase 3 -- gauge residue

- The "three server-side defects" from the 2026-09-28 Rust gauge sweep: re-run
  the Java and Node gauges with `--server rust` and re-triage. One of the three
  was already re-diagnosed as not a `$geoIntersects` bug.
- The Go gauge's 7 remaining failures (backlog triage, 2026-09-30).
- `failCommand` labels and code names: measure against mongod with the
  failpoint enabled (`--setParameter enableTestCommands=1`).

Gauges run in a sub-agent and report counts only (CLAUDE.md, "Tooling").

## Phase 4 -- the embedded server (`secantus_mdb::Server`): DONE (2026-10-06)

`Builder::ttl_sweep` (default 60 seconds, as on the daemon and mongod) and
`Builder::noop_heartbeat` (default off, as on the daemon) run the daemon's two
sweepers. The server joins them before it closes the store. They're tested
through the public API with the official driver.

## Not in this plan

- **`$where` / server-side JavaScript.** It needs an embedded JS engine, which
  is a project of its own. Keep the faithful refusal; decide separately.
- **The authorised decimal last-digit divergence**, which is a decision, not a
  defect.
- **Python-server divergences** (backlog §7.04, the ligature-collation entry).
  They are separate work.
- **Throughput against mongod.** That is the performance track
  (`tasks/rust-perf-findings.md`).

## How each slice lands

- One branch per phase, batched (CLAUDE.md, "Batch several slices"); claim by
  pushing the branch first.
- Each slice ships with:
  - its probe's mongod-vs-Rust count, before and after;
  - a Rust unit test pinning mongod's answer;
  - a gate run: clean-workspace fmt / clippy / test, plus the WT crates from
    their own directories when touched.
- Backlog entries are rewritten or deleted in the same PR.
- A Rust release (`secantusdb-v*`) is cut after each phase that changes the
  wire.

**Rough size (after phase 0):**

| Phase | Estimate |
| --- | --- |
| 1 | about a day |
| 3 | 1-2 days, depending on what the gauge re-triage finds |
| 4 | half a day |

Phase 0 cut the estimate from 5-8 days to 2-4: the list had been read off the
wrong server's section.
