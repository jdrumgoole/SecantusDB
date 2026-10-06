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

## Phase 1 -- the Rust divergences phase 0 found: DONE (2026-10-06)

`remaining_shapes.py` went from 25 of 58 to 3 of 72 (all known: two authorised
last digits and a hash-ordered `w` tag set). Shipped:
- the validation-failure prefix and `errInfo` on every update path;
- validator parsing on `create` / `collMod`;
- `writeConcern.w` parsing and its echo;
- decimal `$log`.

**Two items came off the list unbuilt:** awaitable `hello` and `maxTimeMS`.
Phase 0 measured them against a STANDALONE mongod; against a replica set,
which is what the Rust server presents, both are 0. The probe that found the
extra write-path and `w` shapes was widened in the same slice; see backlog
§7.00.

## Phase 2 -- folded into Phase 1

`$jsonSchema` `errInfo` turned out to be an update-path wiring gap, not the
missing tree builder this plan assumed. The regex, `$project` and positional
items were Python-server entries.

## Phase 3 -- gauge residue: DONE (2026-10-06)

Re-reading the gauge entries before running anything:
- **Go:** the one undiagnosed failure, `heartbeats_processed_more_frequently`,
  was fixed on the Rust server on 2026-09-30. The other six need `$where`, a
  `mongocryptd` binary, or a second replica-set member, all out of scope here.
- **C:** the two `ipv6` failures hard-code `[::1]:27017` (inherent).
- **The "sort by the 2dsphere-indexed field" divergence** from the Java
  gauge's investigation was real and is FIXED. It was a document comparator
  that compared field names before value types, not anything geo. 14 of 28 on
  the Java fixture before, and only the unordered `$geoIntersects` result (no
  order guaranteed) after. `nested_value_sort.py` 4 of 24 -> 0.
- **The Java gauge's three stable failures are gone.** A full run against the
  Rust binary built from this branch (tree `f945d296`), with the `--auth`
  two-phase spawn and one shared daemon, gave 496 / 0 / 404 of 900, with every
  test reporting a result. Both geo classes passed inside it. The committed
  2026-09-30 report already shows 0, so they were fixed between 2026-09-28
  and 2026-09-30.

## Phase 4 -- the embedded server (`secantus_mdb::Server`)

Add builder options for the two sweepers the daemon runs and the embedded
server does not:
- TTL expiry;
- the noop heartbeat, which also prunes the oplog.

The defaults should match the daemon's. Without them, TTL indexes never expire
documents in an embedded server. Test through the crate's public API.

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
