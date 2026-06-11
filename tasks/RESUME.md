# Resume here — Rust-server build-out

Working handoff note for picking the Rust-server work back up on another machine.
Authoritative roadmap stays in `tasks/rust-server-plan.md`; this is the
"where am I, what's next" cheat-sheet.

## Current state (all merged to `main`)

The **Rust server's authentication + TLS surface is complete** (R5), bar legacy
SCRAM-SHA-1:

| Slice | What | PR |
|-------|------|----|
| R5a   | SCRAM-SHA-256 mechanism (`secantus-auth`) | #34 |
| R5b-1 | SCRAM handshake + user mgmt (`saslStart`/`saslContinue`/`createUser`/`dropUser`/`usersInfo`) | #35 |
| R5b-2 | `--auth` gating + RBAC (built-in roles, `check_privilege`, dispatch `authorize`) | #36 |
| R5b-3 | custom user-defined roles (`createRole`/`updateRole`/`dropRole`/`rolesInfo` + resolver) | #37 |
| R5b-4 | role grant/revoke quartet, `updateUser`, `dropAllUsersFromDatabase`, `saslSupportedMechs` | #38 |
| R5c-1 | TLS / mTLS transport in the accept loop (`rustls`, ring) + `peer_cert_dn` plumbing | #39 |
| R5c-2 | MONGODB-X509 auth mechanism (`saslStart` / legacy `authenticate`) | #40 |

`main` is at the R5c-2 merge. The dev branch
`claude/rust-migration-next-steps-h8gjp6` is synced to `main`.

The Rust server now does: encrypted connections, mTLS client-cert verification,
SCRAM-SHA-256 auth, `--auth` command gating, built-in + custom-role RBAC, and
MONGODB-X509 cert auth — all validated by Rust unit tests + pymongo/WT smoke
tests in the `storage-engine` CI jobs.

## R7 — standalone `secantusdb` binary: ✅ DONE

Landed per the plan that used to live here:

1. **`secantus-server::args`** — WT-free, hand-rolled parser (`--host` /
   `--port` / `--storage-path` / `--auth` / `--standalone` / four `--tls-*`
   flags, both `--flag value` and `--flag=value` spellings, TLS pairing rules
   enforced), 11 unit tests in the clean workspace.
2. **`crates/secantusdb`** — WT-linked bin (own `[workspace]`, in the parent
   `exclude`): open `Storage` → `StorageAdapter` → `bind` → print
   `secantusdb listening on <addr>` (flushed; launchers parse it) → block on
   SIGINT/SIGTERM (`ctrlc` + termination feature) → clean `stop()`. Bad args
   exit 2; `--help` / `--version` exit 0.
3. **Smoke**: `tests/test_rust_binary_smoke.py` (pymongo CRUD round-trip +
   clean SIGTERM exit 0, `--standalone` hello shape, bad-args, help) +
   `invoke rust-binary-test` + a `storage-engine` CI step (Linux/macOS;
   Windows bin deferred — see backlog §7 "R7 tail").
4. Plan/backlog updated.

**Heads-up / gotchas learned this session:**
- The bin and `secantus-server-py` link WT → **can't be built in the WT-less dev
  sandbox**; rely on the `storage-engine` CI jobs (iterate via CI like PR #36).
- `secantus-server` itself is WT-free → its arg-parser module + a rustls
  integration test *can* be run locally (`cargo test -p secantus-server`).
- WiredTiger allows only **one open of a home dir per process** — don't write
  tests that stop+reopen the same storage path in one process (bit PR #36).
- Pre-run `ruff format` on any touched `tests/*.py` before pushing (the Python
  `Format check` CI step bit PR #36).
- `**.md` is in the workflow `paths-ignore`, so docs-only commits skip CI.

## After R7 — open threads (pick any)

- **R3b** — tailable change-stream `getMore` (oplog tail → `changestreams::project`,
  `awaitData` blocking). Server/commands layer over WT storage.
- **Storage-backed aggregation** — `$lookup` / `$out` / `$merge` (need storage
  access inside the aggregate handler).
- **R8** — full pymongo conformance gate against the Rust server.
- **SCRAM-SHA-1** (legacy, low priority — no modern driver defaults to it).

## Dev setup reminder (per CLAUDE.md)

- Python 3.12 via `uv`; always `uv run python -m ...`.
- WiredTiger is a vendored submodule (`vendor/wiredtiger`) — `git submodule
  update --init --recursive` on a fresh clone.
- Rust workspace under `crates/` (manifest `crates/Cargo.toml`). The WT-free
  members build with a plain `cargo`; the WT-linked ones need the vendored WT.
- Build/test the WT-free Rust workspace: `cargo test --manifest-path
  crates/Cargo.toml` (run from `crates/` or pass the manifest path).
- `invoke test` / `invoke lint` / `invoke rust-test` / `invoke rust-parity`.
- Branch: develop on `claude/rust-migration-next-steps-h8gjp6`; squash-merge PRs
  to `main`; after merge `git reset --hard origin/main` + force-push the branch.
