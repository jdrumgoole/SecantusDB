# Rust packages for the two Rust servers

**Status:** plan, 2026-09-30; decisions made 2026-10-01 (§2). Both halves of
Phases B, C, D.2 and E are built as of 2026-10-07 (the PG half that day, see
§5.2 / §5.3); what remains is the first real crates.io publish of each line,
which runs from a release tag in Joe's account. Every fact below was read from `origin/main` at `aa0161de` or
measured on this box that day; re-check before relying on one.

## 1. Goal

Give a Rust developer what `pip install SecantusDB` gives a Python one: a
real MongoDB-wire or PostgreSQL-wire server they can start **inside their own
test in one or two lines**, with no external process to manage, plus an
installable binary for CI and containers.

Two user-facing packages, one per server:

| package (crates.io) | library entry point | binary |
| --- | --- | --- |
| `secantus-mdb` | `secantus_mdb::Server::start()` | `secantusd-rs` |
| `secantus-pg` | `secantus_pg::PgServer::start()` | `secantusd-pg` |

The target experience, for each:

```rust
// Cargo.toml: [dev-dependencies] secantus-mdb = "0.6"
#[test]
fn inserts_a_document() {
    let server = secantus_mdb::Server::start().unwrap();   // temp store, free port
    let client = mongodb::sync::Client::with_uri_str(server.uri()).unwrap();
    // ... the server stops and its store is removed on drop
}

// Cargo.toml: [dev-dependencies] secantus-pg = "0.2"
#[tokio::test]
async fn selects_one() {
    let server = secantus_pg::PgServer::start().unwrap();
    let (client, conn) = tokio_postgres::connect(&server.dsn(), NoTls).await.unwrap();
    // ... dropping `server` inside the async test must not panic (see §5.3)
}
```

And `cargo install secantus-mdb` / `cargo binstall secantus-mdb` for the binary.

## 2. Decisions -- made by Joe, 2026-10-01

1. **Names: `secantus-mdb` (MongoDB server) and `secantus-pg` (PostgreSQL
   server).** Both were unclaimed on crates.io on 2026-09-30; register them
   first, before any other work, because a squatter is the one risk here with
   no engineering fix. The binaries keep their names (`secantusd-rs`,
   `secantusd-pg`), and so do the existing `secantusdb-v*` release tags.
2. **Licence: the PG SERVER crate is GPL-2.0-only; `secantus-pgcatalog` and
   `secantus-pgplan` stay Apache-2.0.** `secantus-pgserver` links
   `secantus-core`, `-storage`, `-wt` and `-auth` (GPL-2.0-only) and WiredTiger
   (GPL v2-or-v3), so it is GPL in effect and is now labelled so; the planner
   and catalog link nothing GPL. Relabelling `secantus-pgserver/Cargo.toml` is
   the first change of Phase B.
3. **Internal crates are published**, under the `secantus-` prefix, each with a
   front-page note that it is an implementation detail with no semver promise.
   No folding into the two packages.
4. **Publishing is crates.io trusted publishing from GitHub Actions**, on the
   release tags only; nobody runs `cargo publish` by hand (the PyPI rule).
5. **Two version lines with exact pins.** The MongoDB line (`0.5.x`) and the PG
   line (`0.1.x`) stay separate; every internal dependency is pinned
   `=x.y.z`, and the release tooling rewrites the pins with the versions.
6. **MSRV is the toolchain CI builds with today**, declared as `rust-version`
   in every published crate and raised deliberately later.

## 3. What stands in the way today

Measured, in order of cost:

1. **WiredTiger is prebuilt-only.** `crates/secantus-wt/build.rs` finds WT via
   `SECANTUS_WT_INCLUDE` / `SECANTUS_WT_LIB`, then probes `/tmp/wt-build` and
   `../../build/*/wt-build`, and otherwise panics. It never builds WT. In this
   repo WT is built by the top-level `CMakeLists.txt` (`ExternalProject_Add`),
   which copies `vendor/wiredtiger`, applies the patch scripts in `cmake/`, and
   needs CMake, Ninja, SWIG and Python. None of that is inside a crate, and a
   crates.io package can contain only files inside its own directory.
2. **bindgen needs libclang** for `secantus-wt`, and **`pg_query` 6.2 always
   runs bindgen too** (its `build.rs`, line 70) — so building the PG package
   from source requires libclang whatever we do. The Mongo package can avoid it
   by shipping pre-generated bindings.
3. **The PG server needs a patched `pgwire`.** `[patch.crates-io] pgwire =
   { path = "../vendor/pgwire" }` (`secantus-pgserver/Cargo.toml:72`); the code
   uses `stmt.parameter_oids`, which upstream 0.40.7 lacks
   (`secantus-pgserver/src/lib.rs:2672`, `:27498`). Cargo ignores `[patch]`
   in a published crate, so it would not compile against upstream.
4. **`publish = false`** on all 13 Mongo-side crates, and every internal
   dependency is a path dependency with no `version`.
5. ~~**The PG handle cannot be used inside a tokio runtime.**~~ **Fixed in
   #1665.** Both `bind` (which `block_on`'d its own runtime) and `stop` /
   `Drop` (which shut that runtime down) panicked inside an async context, so
   a `#[tokio::test]` could neither start nor drop the server. The repo's own
   `secantus-pgserver/tests/embedded.rs` had worked around it by keeping every
   call outside `block_on`.
6. **No one-line constructor on either side.** Both servers already have a
   real in-process API — `secantus_server::bind(addr, ServerConfig,
   Arc<dyn Storage>, Arc<CursorRegistry>)` and
   `secantus_pgserver::bind(addr, Storage, Arc<DatabaseRegistry>)` — but a user
   has to assemble storage, the adapter and the cursor registry themselves
   (`secantus-storage-adapter/tests/server_roundtrip_wt.rs` shows the five
   steps). The Python handle does that assembly; Rust needs the same.
7. **docs.rs** builds in a sandbox with limited time; compiling WiredTiger and
   libpg_query there is at risk. Needs a `docsrs` path that skips the native
   build.

What does NOT stand in the way: size. WiredTiger's `src/`, `ext/` and `cmake/`
compress to **3.2 MB** (measured), well under crates.io's 10 MB limit. `test/`
(11 MB), `bench/`, `dist/`, `examples/` and `tools/` stay out.

## 4. Package layout

```
secantus-mdb          (user-facing: Server + bin secantusd-rs)
 ├─ secantus-server   ├─ secantus-commands ─┬─ secantus-core
 │                    │                     ├─ secantus-auth
 │                    │                     └─ secantus-wire
 ├─ secantus-storage-adapter
 └─ secantus-storage ─┬─ secantus-core
                      └─ secantus-wt ── secantus-wiredtiger-sys   (NEW)

secantus-pg           (user-facing: PgServer + bin secantusd-pg; today's secantus-pgserver)
 ├─ secantus-pgplan ── secantus-pgcatalog, pg_query
 ├─ secantus-storage, secantus-core, secantus-auth
 └─ pgwire  (upstreamed change, or `secantus-pgwire` fork — §5.2)
```

`secantus-wiredtiger-sys` is the one new crate: WiredTiger's source,
pre-patched, plus a `build.rs` that compiles it. Separating it means
`secantus-wt` stays a thin safe wrapper and only one crate carries 3 MB of C.
The existing third-party `wiredtiger` / `wiredtiger-sys` crates are not ours
and are not the version or patches we run, so we do not depend on them.

## 5. Work

### 5.1 Phase A — build WiredTiger from a crate (the critical path)

**Status (2026-10-01): steps 1, 2, 3, 5 landed; the gate (6) passes on macOS
arm64 and runs on Linux / Windows in `.github/workflows/wt-sys.yml`; step 4
(pre-generated bindings) landed too: `secantus-wt/src/bindings.rs`, used
when the default `bindgen` feature is off and checked against bindgen when it
is on.** Measured, not estimated: the
`.crate` is 3.2 MB and builds from the tarball in ~26s on an M-series Mac;
`secantus-storage`'s 290 tests pass over it (`--features secantus-wt/bundled`),
and the test binary links only system libraries (`otool -L`). What the spike
found that the plan above did not say:

- The non-Python build needs only two of the five patch scripts (`strict`,
  `musl`), plus two crate-only trims done by `scripts/wt_sys_refresh.py`: the
  bench / example / test / utility `add_subdirectory` lines, and
  `cmake/configs/base.cmake`'s **unconditional, REQUIRED** Python-3
  development probe — without that, the crate build fails on any machine
  lacking Python headers even with `ENABLE_PYTHON=OFF`.
- The copy is gitignored; its digest (`wiredtiger.sha256`) is committed and
  `tests/test_wt_sys_fresh.py` regenerates and compares.
- Compressors come from `libz-sys` (static) and `lz4-sys`; WT's library probe
  is satisfied by pre-setting `HAVE_LIBZ*` / `HAVE_LIBLZ4*` to their headers.
  snappy / zstd / sodium / tcmalloc / memkind are forced OFF so the host's
  installs cannot leak in.
- `cargo package` fails INSIDE the checkout (`No such file or directory`, from
  cargo's git walk over uninitialised submodules); packaging from a staged copy
  works. The publish job must stage the same way.
- `secantus-wt` gains a `bundled` feature (off by default) — the prebuilt path
  is unchanged for the wheel, release workflows and CI.

1. New crate `crates/secantus-wiredtiger-sys` containing a copy of the WT
   source **with the `cmake/patch_wt_*.py` patches already applied** — the
   crate build must not need Python. A repo task
   (`invoke wt-sys-refresh`) regenerates it from `vendor/wiredtiger` + the
   patch scripts, and a test fails if the copy drifts from what the scripts
   produce, so the two WT builds cannot silently diverge.
2. `build.rs` compiles WT with the `cmake` crate: static library, zlib and lz4
   built in (matching `CMakeLists.txt` l.201-205), Python / SWIG / cppsuite
   off. It needs CMake and a C compiler on the user's machine — the same bar
   as `rdkafka` or `rocksdb`, and stated up front in the README.
3. Keep the existing prebuilt path as an OVERRIDE: if `SECANTUS_WT_LIB` is set,
   link that and skip the source build. The wheel, the release workflows and
   every CI job keep working unchanged, and so does every developer who has a
   WT build.
4. Ship **pre-generated bindings** (`src/bindings.rs`, regenerated by the same
   refresh task) so the Mongo package needs no libclang. bindgen becomes an
   optional `bindgen` feature for regenerating them.
5. Compressors: link zlib / lz4 statically from the bundled source rather than
   searching Homebrew — the macOS release binaries once linked
   `/opt/homebrew/opt/lz4/lib/liblz4.1.dylib` (noted at
   `secantus-wt/build.rs:157-167`), which is exactly the non-portable result a
   crates.io build must not produce. Windows builds WT with no compressors
   today (`CMakeLists.txt` l.193); keep that, and document it.
6. **Gate:** `cargo package` the crate, then build that `.crate` tarball in a
   clean container with no repo around it, on Linux, macOS arm64 and Windows.
   That is the only test that proves "nothing outside the crate".

This is also a DATA-FORMAT question, not only a build one: the crate must build
the same WiredTiger (mongodb-7.0.33 plus our patches) the wheel builds, or a
store written by one cannot be trusted to open in the other. The drift test in
step 1 is what protects that, and it is non-negotiable.

### 5.2 Phase B — make every crate publishable

**Status (2026-10-07): both sides are done.** The PG half, 2026-10-07:

- `crates/secantus-pgserver` is now `crates/secantus-pg`, package
  `secantus-pg`, library `secantus_pg`; the binary is still `secantusd-pg`
  and behaves identically. GPL-2.0-only; `secantus-pgcatalog` /
  `secantus-pgplan` stay Apache-2.0. All four PG crates carry crates.io
  metadata, `rust-version = "1.98"`, a README (internal ones with the no-semver
  note) and `=` pins (PG line `=0.1.0-beta.2`, MongoDB line for
  `secantus-core` / `-storage` / `-auth`). The binary's dependencies (`ctrlc`,
  `libc`) sit behind a default-on `bin` feature; the Python handle builds
  with `default-features = false`.
- **pgwire: the fork, not upstreaming.** The vendored copy has diverged well
  past the one `parameter_oids` change step 2 assumed -- 17 files differ from
  upstream 0.40.7 plus a new `messages/fastpath.rs` -- so there is no minimal
  upstream PR to wait on. It moved from `crates/vendor/pgwire` to
  `crates/secantus-pgwire`, is published as `secantus-pgwire` (MIT OR
  Apache-2.0, as upstream; library name still `pgwire`), and the dependents
  say `pgwire = { package = "secantus-pgwire", ... }`. Both `[patch.crates-io]`
  stanzas are gone. Upstreaming the changes is still worth doing; the fork is
  what lets the server publish meanwhile.
- `scripts/crates_package_check.py` has `--line {mdb,pg,all}` (`PG_ORDER`,
  plus `PG_NEEDS`, the MongoDB-side crates a PG release resolves but never
  publishes); `crates-package.yml` checks `all`. `--version` says
  `source: crates.io` for a packaged `secantusd-pg` too.
- docs.rs: the WiredTiger crates skip their native build under `DOCS_RS` as
  before; libpg_query is compiled by `pg_query`'s own build script, which this
  repo cannot skip, so the PG crates' docs.rs build depends on that working
  in docs.rs's sandbox.

What landed for the MongoDB side, 2026-10-01, for the ten crates in
`scripts/crates_package_check.py`'s `PUBLISH_ORDER`:

- `publish = false` gone, `=0.5.3-beta.165` pins on every internal dependency
  (`secantus-wiredtiger-sys` moved onto the Mongo line), crates.io metadata,
  `rust-version = "1.98"`, and a README per crate (the internal ones carry the
  no-semver note). `secantusdb` is renamed `secantus-mdb`; directory, binary
  and tags unchanged.
- `secantus-wt` defaults to `bundled`, so a crates.io build needs neither
  Python nor libclang; `bindgen` is opt-in and `./inv rust-wt-test` enables it.
  The repo's `build/*/wt-build` probe moved into the sys crate, behind the
  override and the bundled source, where a packaged crate never reaches it.
- `--version` says `source: crates.io` for a packaged build.
- DOCS_RS skips both native builds.
- The gate is `scripts/crates_package_check.py` in `crates-package.yml`: it
  stages the crates into one workspace and runs `cargo package --workspace`,
  because `cargo publish --dry-run` resolves siblings from crates.io and
  cannot check anything before the first release.

1. Flip `publish = false` on the crates §2.3 keeps; give every internal path
   dependency a `version = "=x.y.z"` alongside its `path`.
2. `pgwire`: open a PR upstream adding `parameter_oids` (the one local change
   in `crates/vendor/pgwire`). Until it lands and releases, publish the vendored
   copy as `secantus-pgwire` and depend on it by that name. Delete the fork
   when upstream ships.
3. Remove repo-relative assumptions: the `../../build/*/wt-build` probe moves
   behind the override in 5.1.3; the git-tree stamp in the five `build.rs`
   files already degrades to empty without git — make it say `crates.io`
   instead, so `--version` is honest about where a build came from.
4. Add `license`, `repository`, `description`, `readme`, `keywords`,
   `categories`, `rust-version` to every published `Cargo.toml` (the PG crates
   lack `repository` today).
5. `[package.metadata.docs.rs]` with a `docsrs` cfg that skips the native WT
   and libpg_query builds, so documentation builds without them.
6. **Gate:** `cargo publish --dry-run` for every crate in dependency order, in
   CI, on every PR that touches `crates/`. Catches a new path dependency or an
   out-of-crate file the day it is introduced, not on release day.

### 5.3 Phase C — the embedding API

**Status (2026-10-07): both halves are done.** `secantus_pg::PgServer` (step 2)
landed 2026-10-07 in `crates/secantus-pg/src/embedded.rs`: `start()`,
`builder()` with `.storage_path` / `.host` / `.port` / `.databases` /
`.cache_size` (256M default, as the Mongo side), `.dsn()`, `.url()` (the
`postgresql://` form the plan called `connection_string`), `.address()`,
`.port()`, `.stop()`, and `Drop`, which removes a temporary store only once
`RunningPgServer::store_closed()` says WiredTiger closed it. Tests:
`crates/secantus-pg/tests/pg_server.rs` (tokio-postgres, both runtime
flavours, a persistent store across a restart, 50 servers in parallel), a
doc-test, and `examples/quickstart.rs`. The MongoDB half landed 2026-10-01. `secantus_mdb::Server` lives in `crates/secantusdb/src/lib.rs`; the
tests are `crates/secantusdb/tests/embedded.rs` (official driver, tokio both
flavours, 50 in parallel). Not carried over from the daemon: its noop
heartbeat and TTL sweeper threads -- the Python embedded handle runs neither
either; see the backlog entry.

1. `secantus_mdb::Server`:
   - `Server::start()` — temporary store (removed on drop), `127.0.0.1:0`,
     `enable_test_commands: true`, replica-set advertising ON (so change
     streams work, as in the Python default).
   - `Server::builder()` for `.storage_path(p)` (kept on drop), `.port(n)`,
     `.replica_set(None)` for a pure standalone, `.auth(true)`, `.tls(..)`,
     `.cache_size("256M")` — the knobs the Python `RustServer` exposes, with a
     SMALLER default cache than the daemon's 4G, because a test suite starts
     many of these.
   - `.uri()`, `.address()`, `.stop()`; `Drop` stops.
   - Built on the existing `bind` + `Storage::open_with_options` +
     `StorageAdapter` + `CursorRegistry` — the assembly the Python handle does
     in `secantus-server-py/src/lib.rs:80-119`, moved into Rust so both use it.
2. `secantus_pg::PgServer`: the same shape — `start()`, `builder()` with
   `.storage_path`, `.databases([...])`, `.port`; `.dsn()`,
   `.connection_string()` (URL form), `.address()`.
3. ~~Fix the async drop (§3.5).~~ Done in #1665, with current-thread and
   multi-thread `#[tokio::test]` coverage.
4. Feature flags: the library pulls no allocator (`mimalloc` only under the
   `bin` feature — a library must not choose the host program's allocator),
   and the binary sits behind a default-on `bin` feature so a dev-dependency
   user can skip its dependencies (`ctrlc`, `env_logger`).
5. Tests: doc-tests on both entry points; an example per package using the
   official `mongodb` / `tokio-postgres` driver; one test that starts 50
   servers in parallel to catch per-instance resource leaks (WT cache,
   threads, ports) before users do.

### 5.4 Phase D — binaries

1. `cargo install secantus-mdb` / `cargo install secantus-pg` work once A and B
   land (source build; needs CMake, a C compiler, and libclang for PG).
2. `[package.metadata.binstall]` pointing at the tarballs the release workflows
   ALREADY publish (`secantusdb-<ver>-<target>.tar.gz`,
   `secantusd-pg-<ver>-<target>.tar.gz`), so `cargo binstall` fetches a
   prebuilt, PGO-optimised binary with no toolchain. Cheap, and independent of
   the rest — it could ship first.
3. Gaps the release workflows already list and that binstall users will hit:
   no aarch64 Linux, no x86_64 macOS, no musl, and no Windows for the PG
   server.

### 5.5 Phase E — the release pipeline

**Status (2026-10-01): step 1 is in place for the MongoDB crates; steps 2-3
are open.** Done:

- All 15 `secantus-*` names are claimed on crates.io as empty `0.0.0`
  placeholders (Joe's account). Each has a trusted-publishing config for
  `jdrumgoole/SecantusDB`, workflow `publish-crates.yml`, environment
  `crates-io`. Re-run the config script from the session that set it up, or
  add one by hand on crates.io, for any crate that does not have one yet.
- The GitHub `crates-io` environment accepts only the `secantusdb-v*` and
  `secantusd-pg-v*` tags.
- `.github/workflows/publish-crates.yml` runs on `secantusdb-v*` tags. It
  refuses a tag that disagrees with `secantus-mdb`'s version, stages and
  verifies the ten crates with `scripts/crates_package_check.py`, exchanges
  the OIDC token through `rust-lang/crates-io-auth-action`, and runs
  `cargo publish --workspace`, which publishes in dependency order. A
  `workflow_dispatch` defaults to a dry run.

Steps 2-4 landed 2026-10-02: `./inv rust-version-bump --to <ver>`
(`scripts/rust_version_bump.py`) rewrites versions, `=` pins and lockfiles and
checks `--locked` still resolves; publishing is per crate and resumable
(`scripts/crates_publish.py` skips versions already on crates.io); and the
`secantusdb-release` skill documents the publish step, the yank procedure and
that a fix is always a new version. Phase D.2 landed with them: binstall
metadata on `secantus-mdb` points at the release archives.

The PG line joined on 2026-10-07: `publish-crates.yml` also fires on
`secantusd-pg-v*` tags, checks the tag against `crates/secantus-pg`'s version,
and publishes the four PG crates (`crates_publish.py --line pg`, which refuses
to start if a MongoDB-side crate they pin is not on crates.io yet);
`./inv rust-version-bump --line pg --to <ver>` bumps the PG line; binstall
metadata on `secantus-pg` points at the `secantusd-pg-<ver>-<target>.tar.gz`
archives.

**Both lines are published (2026-10-07).** `secantusdb-v0.5.3-beta.173` and
then `secantusd-pg-v0.1.0-beta.3`, the PG line's first: all four PG crates
went up through trusted publishing on the first run, so every `secantus-*`
name has a working config. The order matters and will again: the PG crates
pin the MongoDB-side crates exactly, so when `secantus-storage`,
`secantus-wiredtiger-sys` or another MongoDB-side crate has changed since its
last publish, bump and tag that line FIRST, or a crates.io build of
`secantus-pg` is built from older storage than the released binary.

1. A `publish-crates.yml` workflow on the existing `secantusdb-v*` /
   `secantusd-pg-v*` tags: verify the tag against the crate version (as the
   binary workflows do), then `cargo publish` each crate in dependency order,
   waiting for the index between them.
2. The version bump at release time rewrites the exact-pinned internal
   versions too (today it is a `sed` over one version string; §2.5 adds the
   `=x.y.z` pins to what it must rewrite).
3. Teach the `secantusdb-release` skill the new step; never `cargo publish`
   by hand, the same rule as PyPI.
4. A yanked or broken release: document the yank procedure and that crates.io
   versions are immutable — a fix is always a new version.

### 5.6 Phase F — documentation

A README per user-facing crate (it is the crates.io page), a
`docs-rust/embedding-rust.md` page next to the Python-embedding one, and the
two server pages on the website gaining a "Rust" install tab. The build
prerequisites (CMake, C compiler, libclang for PG) go in the FIRST paragraph,
not a footnote.

## 6. Order and size

| phase | depends on | rough size |
| --- | --- | --- |
| §2 decisions | — | **made** 2026-10-01 |
| C.3 async-drop fix | — | **done** (#1665) |
| D.2 binstall metadata | names registered | small |
| A — WT from a crate | — | **largest and riskiest**: 3-5 days, mostly CI across three OSes |
| B — publishable crates | A, §2 | 2-3 days (pgwire upstreaming may take longer; the fork covers it) |
| C — embedding API | B | 2-3 days |
| E — pipeline | B | 1-2 days |
| F — docs | C | 1 day |

These are estimates from reading, and CLAUDE.md records how those go; Phase A
in particular should be sized from a spike — build WT from a packaged crate on
one OS — before anyone commits to a date.

## 7. Risks

- **Two WiredTiger builds.** The wheel's CMake build and the crate's must not
  drift; the drift test in 5.1.1 is the guard. A divergence is a storage-format
  risk, not a build nuisance.
- **Build prerequisites put people off.** CMake + C compiler (+ libclang for PG)
  is a real bar; binstall (D.2) is the answer for anyone who only wants the
  binary, and the README must say so first.
- **Publishing internals freezes nothing but invites use.** The "no semver
  promise" note has to be on each internal crate's front page.
- **The PG package's size of surface.** It carries libpg_query (C, compiled
  from source) and WiredTiger; a first build will take minutes. State it.
- **GPL.** Both packages are GPL in effect (WiredTiger). A dev-dependency used
  only in tests is not distributed with the user's product, which is why this
  is workable for the stated audience — but the README should say plainly what
  the licence is rather than leave users to find WT's.
