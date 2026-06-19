# Phase R — PITR for the Rust server (implementation plan)

Status: **proposed, for review.** Scopes porting point-in-time recovery to the
Rust server so the standalone `secantusdb` binary can back up and restore, at
parity with the Python server (which completed PITR in 0.5.4b4–b6:
`src/secantus/{oplog_replay,pitr_archive,diff}.py`).

## Current Rust state (verified)

The backlog's stated prereqs are **partly already done** — re-scoped here against
the actual code:

| Capability | State | Location |
|---|---|---|
| DDL oplog `c` entries (create/createIndexes/dropIndexes/collMod/drop/dropDatabase) | **DONE** (R0a) | `crates/secantus-storage/src/lib.rs` (create_collection 2071, create_index ~2786, drop_index ~2859, coll_mod ~1295, drop_collection ~2102, drop_database ~2135) |
| Update oplog uses `{$v:2, diff}` | **DONE** | `lib.rs:~3786` |
| `compute_update_description` (forward diff) + parity | **DONE** (R0b ½) | `crates/secantus-core/src/diff.rs:115`; `tests/test_rust_diff_parity.py` |
| Oplog read API (read_oplog, oplog_floor_seq, read_preimage, find_seq_for_ts, oplog_tail_seq, prune_oplog) | **DONE** | `lib.rs:1220/1256/1332/1442/1269/1352` |
| `apply_update_description` (reverse diff, needed by replay) | **ABSENT** (R0b ½) | — |
| WT `backup:` cursor + `create_archive`/`extract` | **ABSENT** (R0c) | only `checkpoint()` at `crates/secantus-wt/src/lib.rs:213` |
| Oplog applier + replay/`replay_mode` | **ABSENT** | only `set_enable_oplog` toggle at `lib.rs:1034` |
| `secantusdb restore` subcommand | **ABSENT** | binary is server-only, hand-rolled args in `crates/secantus-server/src/args.rs` |
| create_collection carries options in oplog | **ABSENT** (parity gap with Python 0.5.4b4) | `create_collection(&self, db, coll)` `lib.rs:2071` — no options param |

**Strategic shortcut (cross-server format identity).** Both servers share the
exact WT schema + BSON oplog shape. So the *existing Python* `secantusdb restore`
already restores a Rust server's data dir/backup once the Rust server can emit one
(R0c). Native Rust restore (R0b-reverse + applier + subcommand) is still in scope
for a self-contained binary, but R0c alone unlocks PITR-of-Rust-data via Python.

## Work items (ordered, each its own slice + Rust version bump)

**R1 — WT backup cursor + archive (R0c).**
- Expose a `backup:` cursor in `secantus-wt` (enumerate the consistent file set).
- Implement `Storage::create_archive` (checkpoint → tar.gz the backup file set →
  embed `pitr-manifest.json`) + `extract_backup_archive`, mirroring
  `storage.py:create_archive` / `_pitr_manifest`. Wire `secantusAdmin.backupArchive`
  into the Rust command layer.
- *Unlocks: Python can already restore Rust backups → cross-server smoke (R6a).*

**R2 — `apply_update_description` in Rust (R0b remainder).**
- Port the reverse-diff apply into `secantus-core/src/diff.rs` (inverse of the
  existing `compute_update_description`).
- Extend `tests/test_rust_diff_parity.py` to cover apply (round-trip:
  pre → compute → apply == post), against the Python oracle in `tests/test_diff.py`.

**R3 — oplog applier + replay in `secantus-storage`.**
- Port `_apply_entry` (i/u/d/c dispatch) + `replay`/`restore_to_timestamp`,
  reusing existing read_oplog/oplog_floor_seq/find_seq_for_ts. Replay suppresses
  emission (reuse `set_enable_oplog(false)` or a scoped replay flag).
- Mirror collection-options carry: give Rust `create_collection` an options
  argument so the `create` oplog `c` carries capped/size/validator (parity with
  Python 0.5.4b4) — required for faithful replay.

**R4 — `secantusdb restore` subcommand.**
- Add a `restore` mode to the binary (hand-rolled args like the server):
  `--source --target-dir [--to-time | --to-timestamp] [--preserve-oplog]`.

**R5 — PITR enhancements for parity (optional, after MVP).**
- `import_oplog_segment` + `--preserve-oplog` carry (Python 0.5.4b5).
- v2: `oplog_archive_dir` prune-archiving + `archive_base_snapshot` +
  `restore_from_archive_dir` + archive-dir source detection (Python 0.5.4b6).

**R6 — cross-server restore parity smoke.**
- (a) Rust server produces a backup → Python `oplog_replay` restores it → assert
  data (lands after R1).
- (b) Python backup → Rust `restore` subcommand → assert data (lands after R4).
- Add to the gauge/parity harness; both directions must be byte-faithful.

## Progress

- **R6a — DONE (no Rust code needed).** `tests/test_rust_pitr_cross_server.py`
  proves the Python `oplog_replay` restore tool rebuilds a database from a
  *stopped* Rust server's data directory — both "latest" and bounded
  "restore to a mark". This confirms the WT-schema + oplog-shape identity
  empirically: **PITR already works for Rust-server data via the Python tooling.**
  R1 (native Rust `create_archive`) is therefore a convenience (single-file
  backups / a self-contained binary), not a blocker for cross-server recovery.

## Sequencing & MVP

- **MVP = R1 + R6a:** Rust can back up; Python restores it. Smallest path to
  "PITR works for Rust-server data." *(R6a met; the stopped-data-dir path needs
  no Rust backup command at all.)*
- **Native = R2 → R3 → R4 → R6b.** Self-contained Rust backup+restore.
- **Full parity = R5.**

## Versioning / testing

- Each slice bumps every `crates/*/Cargo.toml` in lockstep (`0.5.2-beta.N` →
  N+1; a patch bump resets beta to 0). See CLAUDE.md "Versioning".
- Build/test via `invoke rust-build` / `rust-test` / `rust-parity`; needs the
  Rust+WT build env (libclang/WT — see memory `rust-wt-local-build`).
- Gates: parity suites stay green; both servers' pymongo + driver gauges
  non-regressing; new cross-server smoke green.
