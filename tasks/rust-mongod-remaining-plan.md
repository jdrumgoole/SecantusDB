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

## Phase 0 -- re-measure before building (half a day)

The backlog lags reality, and several entries below were measured against the
WRONG server or have probably been fixed since:

- The awaitable-`hello` entry (25 of 33) was measured with `PROBE_SERVER` at a
  **Python** server.
- The `maxTimeMS` entry (8 of 11) does not say which server it measured.
- These §7.00 entries should already have shown up in today's 6,628-case
  expression sweep, and did not:
  - `$slice` with a negative position;
  - `$bucketAuto` with a decimal;
  - decimal `$pow`;
  - "Decimal128 refused by some operators";
  - "50 codes + 212 messages" on the error surface.

Steps:

1. Build `secantusd-rs` from `origin/main` (provenance-checked), and start
   `mongod` 8.2.11 on a spare port.
2. Run each probe with `PROBE_SERVER` at the Rust binary:
   - `max_time_expiry.py`
   - `awaitable_hello.py`
   - `regex_value_semantics.py`
   - `change_streams.py` / `change_stream_fuzz.py`
3. Write two small new probes for the shapes that have none: `$jsonSchema`
   (type keywords, failure `errInfo`) and write concern (`errInfo`, unknown
   `w` tag).
4. Rewrite each backlog entry with the Rust count, or close it.

**Exit:** every item below carries a fresh Rust-vs-mongod count, and the plan
is re-ordered by it. Anything at 0 is closed, not built.

## Phase 1 -- what drivers see on every connection

**1a. Awaitable `hello`** (only if Phase 0 shows Rust diverging). The backlog
entry lists the shapes:
- a malformed `topologyVersion` / `maxAwaitTimeMS` accepted;
- a stale or foreign topology held rather than answered at once;
- the first streamed reply not waited.

Probe: `awaitable_hello.py`. Gauge to rerun: Go (`heartbeats_processed_more_frequently`).

**1b. `maxTimeMS` expiry replies.** The entry says "no executor prefix on
find / aggregate / distinct / count", which is likely message text only.
Match code, message and prefix per command. Probe: `max_time_expiry.py`.

**1c. Write-concern errors.**
- `writeConcernError.errInfo` should be present.
- An unknown `w` tag should fail the way mongod does, not as a pre-flight
  refusal.

New probe from Phase 0. Gauges to rerun: Java, Node (write-concern spec tests).

## Phase 2 -- query and validation correctness

**2a. `$jsonSchema`.**
- `type: "integer"` should be refused (mongod: 9).
- A failed validation should carry mongod's `errInfo.details` (`schemaRulesNotSatisfied` tree), not just `{operatorName}`. This is the larger half: the tree has a fixed shape per keyword, so measure it keyword by keyword.

**2b. Regex anchors.** `\Z` should match before a final newline; `\z` should
be accepted. mongod uses PCRE2; check whether the regex crate's translation
layer or a rewrite of the two anchors is the fix. Probe: `regex_value_semantics.py`.

**2c. Aggregate `$project: {_id: 1}`.** Returns whole documents. Probably a
one-line inclusion-detection bug; reproduce first.

**2d. Positional `a.$` in change events.** The update description for
`$set: {"a.$": 9}` (backlog 7.02). Probe: `change_streams.py`.

## Phase 3 -- gauge residue

- The "three server-side defects" from the 2026-09-28 Rust gauge sweep: re-run
  the Java and Node gauges with `--server rust` and re-triage. One of the three
  was already re-diagnosed as not a `$geoIntersects` bug.
- The Go gauge's 7 remaining failures (backlog triage, 2026-09-30).
- `failCommand` labels and code names: measure against mongod with the
  failpoint enabled (`--setParameter enableTestCommands=1`).

Gauges run in a sub-agent and report counts only (CLAUDE.md, "Tooling").

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

**Rough size:**

| Phase | Estimate |
| --- | --- |
| 0 | half a day |
| 1 | 1-2 days |
| 2 | 2-3 days (`$jsonSchema` `errInfo` dominates) |
| 3 | 1-2 days, depending on what the re-triage finds |
| 4 | half a day |

Phase 0 will move these numbers; sizes from reading have been wrong in both
directions here before.
