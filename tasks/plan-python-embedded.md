# Plan: an in-process `MongoServer` in the PyPI wheel

Status: **agreed, 2026-11-19** (Joe answered the four open questions the same day; see the end).
Phases 1-4 built on branch `python-embedded` (2026-11-19); phase 5 is the
first release through it. See "As built" at the end for where the build
departs from the text below. Asked for by Joe on 2026-11-19: "make it
the default for python, the subprocess launcher should be an option".

## Goal

`pip install SecantusDB` gives a `MongoServer` that runs `secantusd-mdb`
**inside the Python process** by default, the way SQLite does, with the
current child-process launcher kept as an option:

```python
from secantusdb import MongoServer

with MongoServer() as server:                    # in-process (new default)
    client = pymongo.MongoClient(server.uri)

with MongoServer(embedded=False) as server:      # child process (today's behaviour)
    ...
```

The server still listens on a real loopback TCP port and pymongo still
connects over it. "Embedded" means no child process, not no socket: that is
what keeps a pymongo client unable to tell it from `mongod`.

## What exists today

- **The extension is built, tested and loads.** `crates/secantus-server-py`
  builds `_secantus_server`, an `abi3-py310` PyO3 `cdylib`. `RustServer(storage_path, port, …)`
  runs the whole server in-process: the accept loop runs on a native thread
  that does not hold the GIL, and Python keeps only a lifecycle handle
  (`address`, `uri`, `stop`, context manager). The repo's tests use it
  (139 call sites under `tests/`). The glibc "cannot allocate memory in static
  TLS block" import failure is already fixed (`mimalloc` `local_dynamic_tls`).
- **The release job already compiles it, then throws it away.**
  `release-binaries.yml` runs the storage-engine wheel build only to get a
  static WiredTiger for the binary. That wheel contains `_secantus_server`,
  built in `fast` mode (no PGO, no LTO, 16 codegen units), and is discarded.
- **The PyPI wheel compiles nothing.** `publish-pypi.yml` +
  `packaging/pypi/build_wheels.py` repackage the released `secantusd-mdb`
  and `secantusd-pg` archives (sha256-checked) into one `py3-none-<platform>`
  wheel per platform (Linux x86_64, macOS arm64), with the pure-Python
  `secantusdb` launcher that runs the binaries as child processes
  (`packaging/pypi/secantusdb/_server.py`).
- **A rule this reverses.** CLAUDE.md, "Distribution": "The embedded
  `RustServer` / `PgServer` handles remain a SOURCE-BUILD tool for this
  repo's tests -- never document them as something `pip install` provides."
  This plan changes that rule (Joe's request, 2026-11-19), so the CLAUDE.md
  paragraph and the `secantusdb-release` skill change with it.

## Design

### 1. Where the extension is built: the MongoDB release job (recommended)

Build `_secantus_server` in `release-binaries.yml`, per target, beside the
binary, and attach it to the `secantusdb-v*` GitHub Release as
`secantusdb-ext-<version>-<target>.tar.gz` + `.sha256`.

- `publish-pypi.yml` keeps its property: **it compiles nothing.** It
  downloads one more checksummed archive and repackages it, the same as the
  binaries.
- The extension and the binary come from the same commit, the same
  WiredTiger build and the same release smoke test.
- Release-job cost: one more `cargo build` per target against the WiredTiger
  already built there. Linux and macOS only (the wheel's platforms). Windows
  keeps its `fast`-mode throwaway build untouched.
- The alternative, compiling in `publish-pypi.yml`, builds WiredTiger a
  second time and puts an unreleased artifact into the wheel. Rejected.

Build flags: `--release --locked --no-default-features` (MongoDB only; see
open question 1), with LTO and one codegen unit as the crate's
`[profile.release]` sets. PGO: see open question 4.

Release-job check, before upload: in a clean venv on the runner,
`import _secantus_server`, start a `RustServer` on port 0, insert and read
one document with pymongo, then stop it. This proves the artifact *loads*,
which building it does not (the static-TLS incident).

### 2. The wheel

`build_wheels.py`:

- Reads the new archive, checksum required, as it does for the binaries.
- Installs the module **inside the package**, as
  `secantusdb/_secantus_server.abi3.so`, imported as
  `secantusdb._secantus_server`. PyO3's `PyInit__secantus_server` matches the
  last name component, so no rename is needed, and nothing lands at the top
  level of `site-packages`.
- Changes the wheel tag from `py3-none-<plat>` to `cp310-abi3-<plat>`. The
  platform tag is still read from the binaries, now including the extension
  (newest GLIBC symbol / Mach-O minimum).
- Keeps both binaries in `secantusdb/bin/` (the subprocess mode and the
  `secantusd-mdb` / `secantusd-pg` console scripts need them).
- Wheel size: about +10 MB per platform (the release archives are 10-11 MB).

**Fallback wheel (open question 2):** also emit today's `py3-none-<plat>`
wheel, with no extension. pip prefers `cp310-abi3` wherever it applies and
picks `py3-none` only where abi3 cannot load, chiefly free-threaded CPython
(3.13t / 3.14t), where `abi3` is not supported.

### 3. The launcher API

`packaging/pypi/secantusdb/_server.py`:

- `MongoServer(storage_path=None, *, port=0, startup_timeout=60.0,
  embedded=True)`. The attributes stay the same (`uri`, `host`, `port`,
  `storage_path`, `start()`, `stop()`, context manager), so existing code
  changes behaviour, not API.
- `embedded=True`: `start()` creates the temp dir as now, then
  `secantusdb._secantus_server.RustServer(str(path), port)`; `host`/`port`
  come from `.address`. `stop()` calls `.stop()` and then removes the temp
  dir, as now. A start failure raises `ServerStartError` with the native
  error's message chained, never a bare PyO3 exception.
- `embedded=False`: today's child process, unchanged.
- **The extension is missing** (fallback wheel, or a source checkout of the
  launcher): `MongoServer()` raises `ServerStartError`, naming the reason and
  `embedded=False`. **No silent fall back to a subprocess** (open question 2):
  a different process model than the one asked for is a different
  durability and failure story, and the caller should choose it.
- `output`: the child's last lines in subprocess mode. In-process the server
  logs to the process's stderr; `output` returns `""` and the docstring says
  where the log went.
- **Fork.** WiredTiger's threads do not survive `fork()`. The launcher
  registers `os.register_at_fork(after_in_child=...)`, which marks every
  running embedded server in the child as unusable. A `stop()` or `uri` in
  the child raises a clear error instead of touching an inherited WiredTiger
  connection, and the docs say to use `spawn` multiprocessing (the macOS
  default) or start the server after forking.
- `PostgresServer` is unchanged in phase 1 (subprocess only; open
  question 1). Its signature gains `embedded=False` only once an in-process
  PG server ships, so it never accepts an argument it cannot honour.

### 4. What the in-process mode changes for users

These go in the docs, in this order:

1. **One process's fate.** A WiredTiger panic or a Rust abort ends the
   Python process, where a child process would have died alone. That is
   the honest consequence of a database in your process (SQLite's model
   too). `embedded=False` is the answer for anyone who needs isolation.
2. **Fork**, as above.
3. **Several servers** may run in one process on different `storage_path`s
   (one WiredTiger connection each). Two on the same path is refused by
   WiredTiger's lock, the same as two daemons.
4. Start-up is a library call: no process spawn, no waiting for a banner.

## Tests

- `packaging/pypi/smoke_test.py` (runs on the installed wheel in
  `publish-pypi.yml`'s smoke job, Linux and macOS), for both modes:
  start, insert, find, stop, temp dir removed.
- The same file, in-process only:
  - two servers at once;
  - stop and restart on the same `storage_path` in one process, with the data
    still there (a WiredTiger close and reopen in one process);
  - a server that fails to start raises `ServerStartError`;
  - in a forked child, using the parent's server raises rather than touching
    WiredTiger.
- On the fallback wheel: `MongoServer()` raises the named error, and
  `MongoServer(embedded=False)` works.
- `tests/test_pypi_wheel_build.py`: the wheel tag is `cp310-abi3-*`; the
  extension is in the package and covered by RECORD; a missing or
  bad-checksum extension archive is refused; the fallback wheel holds no
  extension.
- The release-job load check (section 1).

A test that fails or hangs here is a release blocker, not a skip (CLAUDE.md,
"Never ignore or discount an error").

## Docs and records

- `docs-rust/embedded.md`, "From Python": in-process is the default,
  `embedded=False` is the option, then section 4's four points. The
  `RustServer` section for source builds stays.
- `packaging/pypi/README.md` (the PyPI page), and `secantusdb/__init__.py`'s
  docstring.
- CLAUDE.md "Distribution": the wheel carries the in-process extension; the
  SOURCE-BUILD-only rule is retired with this decision's date.
- `secantusdb-release` skill: the new release asset and the order (the
  extension archive must exist before `publish-pypi.yml` runs).
- `changelog.d/` fragment.
- Talk deck: the usage slide's speaker notes can then say "in-process from
  Python and Rust".

## Phases (one branch and PR each, `release-binaries.yml` first)

1. **Release job:** build, load-check and attach the extension archive.
   Prove it with a `workflow_dispatch` dry run on a tag before relying on it.
2. **Wheel:** `build_wheels.py` packs it, the tag change, the fallback wheel,
   and `test_pypi_wheel_build.py`.
3. **Launcher:** `embedded=True` default, `embedded=False`, the fork guard,
   errors, `smoke_test.py`.
4. **Docs and records** (the section above).
5. **First release through it:** `pip install` from PyPI into a clean venv
   on both platforms, both modes, before the website or the deck says so.

Phases 2 and 3 must land together in one release. A wheel whose launcher
defaults to an extension the wheel does not ship would fail every
`MongoServer()`.

## Open questions for Joe (recommended answer first)

1. **PostgreSQL.** The extension's default `pgserver` feature also builds
   `PgServer`. The wheel's PostgreSQL binary comes from the newest
   `secantusd-pg-v*` release, but an extension built at the `secantusdb-v*`
   tag would carry the PG code of *that* commit, so embedded and subprocess
   `PostgresServer` would run different PostgreSQL server versions.
   - **(a) Recommended:** MongoDB only in phase 1 (`--no-default-features`);
     `PostgresServer` stays a subprocess, and the PG half follows as its own
     plan.
   - **(b)** Build the PG half at the PG release, in a second extension.
     This puts two statically linked WiredTigers in one process, which needs
     its symbol visibility proved first.
   - **(c)** Ship it now and accept the version skew.
2. **Pythons the extension cannot serve** (free-threaded 3.13t / 3.14t).
   **Recommended:** publish the `py3-none` fallback wheel too, and have
   `MongoServer()` raise a named error there rather than silently start a
   subprocess. The alternatives are no wheel at all for those Pythons
   (`pip install` fails), or a silent fallback.
3. **Argument name.** **Recommended:** `embedded=True|False`. The
   alternatives are `subprocess=False|True`, or
   `mode="in-process"|"subprocess"`.
4. **PGO for the extension.** **Recommended:** an on-target two-stage
   profile in the release job, as the binary gets (the most faithful build),
   sequenced after phase 1 if it slows the first release. The committed
   wheel profile (`crates/pgo/_secantus_server.profdata.tar.gz`) is the
   cheaper stopgap.

## Decisions (Joe, 2026-11-19)

All four recommendations taken:

1. **MongoDB only in phase 1.** The extension is built with
   `--no-default-features`, `PostgresServer` stays a subprocess, and
   in-process PostgreSQL gets its own plan.
2. **Fallback wheel, explicit error.** Publish the `py3-none` wheel too.
   Where the extension is absent, `MongoServer()` raises a named
   `ServerStartError` pointing at `embedded=False`, never a silent
   subprocess.
3. **`embedded=True|False`** is the argument.
4. **On-target two-stage PGO** for the extension, sequenced after phase 1.
   Phase 1 ships without PGO (LTO only) rather than wait for it.


## As built (2026-11-19)

Phases 1-4 in one PR, since phases 2 and 3 must ship together and phase 1's
dry run needs the workflow on a branch anyway. Where the build differs from
the design above, and why:

- **The daemon's defaults, not the handle's.** `RustServer`'s own defaults
  are the repo tests' (no replica set, test commands on, TTL monitor off).
  The launcher passes `replica_set_name="secantus"` (Joe, 2026-11-19: "We
  need it to be a replica set for change streams"),
  `enable_test_commands=False` (a new `RustServer` argument, default `True`
  so the repo's tests are unchanged) and `ttl_sweep_seconds=60`, so
  `embedded=True` and `embedded=False` behave alike.
- **`stop()` closes the store and raises on failure.** `RustServer.stop()`
  now closes the WiredTiger connection explicitly, in the daemon's order, and
  raises if the final checkpoint fails (the daemon's RM-189 behaviour).
  Without it, an in-process failure would have been a stderr line, and
  reopening the same path in one process would depend on drop order. It
  closes only when it holds the last reference: a connection thread that
  outlived the bounded drain makes `stop()` raise rather than close
  WiredTiger under it.
- **Fork.** `uri` stays readable in a forked child, because connecting to
  the parent's server over TCP is a legitimate use and touches no
  WiredTiger state. Leaving the parent's `with` block in the child is a
  no-op, because unwinding a `with` block is not a request to stop. Only an
  explicit `stop()` (or `start()`) in the child raises `ForkedServerError`.
  The child's copy of the native handle is leaked with `mem::forget`
  (`_abandon_after_fork`), so its drop at interpreter exit never joins
  threads that do not exist.
- **Old tags cannot be repackaged.** `build_wheels.py` requires the
  extension archive, so `publish-pypi.yml` dispatched on a tag from before
  this change fails by name. It used to be able to repackage any tag.
- **Logging.** The extension installs no Rust logger, so the server's `log`
  records are not printed in-process. Its `eprintln!` lines and WiredTiger's
  messages go to the process's stderr. The docstring says only that much.
  Routing the records into Python's `logging` (pyo3-log) is a follow-on.

Rehearsed locally (macOS arm64, 2026-11-19): `build_wheels.py` over the real
`secantusdb-v0.5.3-beta.198` and `secantusd-pg-v0.1.0-beta.20` archives plus
a locally built extension archive. uv resolved CPython 3.10 and 3.12 to the
abi3 wheel and free-threaded 3.13t to the fallback, and `smoke_test.py`
passed on all three.
