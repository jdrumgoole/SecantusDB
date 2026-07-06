# Changelog

All notable changes to SecantusDB are documented here. This file is the
**system of record** for what shipped in each release — the per-release
blog posts on [secantusdb.com](https://secantusdb.com/categories/releases.html)
are generated from these entries via `tools/generate_blog_post.py`.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
with one extension: each release carries a one-to-three-paragraph **prose
lede** between the date line and the structured `#### Added` /
`#### Changed` / `#### Fixed` subsections. The prose lede is what the
blog generator lifts verbatim as the marketing-post body, so it should
read as a self-contained narrative — not as "v0.5.1bN ships X."

This project adheres roughly to [Semantic
Versioning](https://semver.org/spec/v2.0.0.html), but while we're in
beta the patch number `bN` rolls forward on every PyPI-visible push;
the API surface itself is shaped by Semantic Versioning intent.

## [Unreleased]

### Rust server: `$stdDevPop` / `$stdDevSamp` group accumulators

The Rust server now supports the `$stdDevPop` and `$stdDevSamp` accumulators in
`$group` (and `$setWindowFields`), matching the Python server — population
standard deviation (÷n, `0` for a single value) and sample standard deviation
(÷n-1, `null` for fewer than two values). Previously the Rust server rejected
these accumulators; the Python server already had them.

To keep the two engines bit-for-bit identical, both now compute the deviation with
the same fixed sequence of correctly-rounded IEEE operations — a naive left-fold
float sum, multiply-based squaring, and hardware `sqrt`. CPython 3.12's `sum()`
builtin switched to Neumaier *compensated* summation for floats, which is more
accurate but would diverge from the Rust engine's naive fold by a last ULP, so the
Python `_std_dev` now sums with an explicit loop instead of `sum()`. (mongod uses
an online Welford-style algorithm, so neither server matches it to the last ULP;
aligning the two SecantusDB engines is the goal.) A parity fuzz seed caught the
divergence before it shipped.

#### Added

- `secantus-core` (`group.rs`): `$stdDevPop` / `$stdDevSamp` accumulators (numeric
  values folded to `f64`, non-numeric defers to Python), shared by `$group` and
  `$setWindowFields`.

#### Changed

- `aggregate.py` (`_std_dev`): compute with a naive float fold + multiply + `sqrt`
  (no `sum()` / `** 2` / `** 0.5`) so the Python and Rust engines agree bit-for-bit.

### Stable `hello` topologyVersion — no more spurious connection-pool churn (both servers)

`hello`'s `topologyVersion.processId` was minted fresh on every call
(`ObjectId` from `now()` on the Python server, `ObjectId::new()` on the Rust
server). The SDAM spec treats a *changed* `processId` as "the server restarted",
so drivers reacted to nearly every monitoring heartbeat by invalidating and
clearing the connection pool — closing the live connection and reconnecting.
Both servers now pin `processId` once per process, so a driver sees a stable
topology and keeps its connections. This was surfaced by the Java driver's
connection-pool-logging and client-metadata event-count tests (which observed
the extra `connectionClosed` / "Connection pool cleared" events), but the churn
affected every driver's SDAM.

#### Fixed

- **Python server** (`commands.py`) and **Rust server** (`secantus-commands`
  `handshake.rs`): `hello` returns a process-stable `topologyVersion.processId`
  instead of a fresh ObjectId per call. Regressions:
  `tests/test_hello_topology.py` (pymongo, both processId stability and
  cross-connection identity) and a `secantus-commands` unit test.

### `$dateToParts` honours a `timezone` (both servers)

`$dateToParts` now accepts a `timezone` and returns the year/month/day/hour/…
parts read off the wall clock in that zone, instead of always in UTC.
`{$dateToParts: {date: "$d", timezone: "America/New_York"}}` on a `16:30Z` instant
returns hour `11` in winter (EST); `Asia/Tokyo` rolls the day forward. Fixed-offset
zones (`+05:30`, `UTC`) and named IANA zones both resolve; no `timezone` still
reads UTC. Like the date extractors, this was a gap in **both** servers — each
previously ignored `timezone` on `$dateToParts`.

The Rust server resolves named zones via the shared `timezone_offset_ms` helper
(the unambiguous instant→wall-clock direction, matching Python `zoneinfo`); an
unknown zone name defers to the Python oracle. `$dateTrunc` / `$dateDiff` timezone
remain deferred — `$dateTrunc` truncates to a *local* boundary that must convert
back to an instant (local→instant, DST-ambiguous), and `$dateDiff`'s `day`/`week`
already count elapsed duration rather than local-calendar boundaries, so timezone
there would compound an existing divergence (see `tasks/backlog.md` §7.5).

#### Added

- `expressions.py` / `secantus-core`: `$dateToParts` reads `timezone` (fixed-offset
  + named IANA), shifting the instant into the zone before splitting into parts.
  The Rust side factors the fixed-offset/named-zone resolution into a shared
  `timezone_offset_ms` helper now used by `$dateToString`, the `{date, timezone}`
  extractors, and `$dateToParts`.

### Date component extractors honour a `timezone` (both servers)

The date component operators — `$year`, `$month`, `$dayOfMonth`, `$dayOfWeek`,
`$hour`, `$minute`, `$second` — now accept mongod's `{date: <expr>, timezone:
<expr>}` object form, reading the component off the wall clock in the requested
zone rather than always in UTC. `{$hour: {date: "$d", timezone:
"America/New_York"}}` on a `16:30Z` instant returns `11` in winter (EST) and `12`
in summer (EDT); a shift that crosses midnight moves `$dayOfMonth` too. Both
fixed-offset zones (`+05:30`, `UTC`) and named IANA zones resolve, and a bare date
expression still reads UTC as before.

This closes a conformance gap that was present in **both** servers — previously
each ignored `timezone` on these operators and a `{date, timezone}` argument
silently returned null. The Rust server resolves named zones via the same
`chrono-tz` path as `$dateToString` (the unambiguous instant→wall-clock direction,
matching Python `zoneinfo`); an unknown zone name defers to the Python oracle.

#### Added

- `expressions.py` / `secantus-core`: a shared date-operand resolver
  (`_date_operand` / `date_operand_millis`) that both the bare-date and
  `{date, timezone}` object forms of the seven date component extractors route
  through, shifting the instant into the requested zone before the component is
  read. Fixed-offset and named IANA zones both supported on both servers.

### Rust server: `$dateToString` now formats dates in named IANA timezones

The Rust server can now render `$dateToString` in a named IANA timezone —
`{$dateToString: {date: "$d", timezone: "America/New_York"}}` — with the correct
daylight-saving offset for the instant being formatted, exactly as the Python
server (and real mongod) do. A summer date shifts by `-04:00` (EDT), a winter one
by `-05:00` (EST); `Europe/Dublin`, `Asia/Tokyo`, and every other zone resolve the
same way. Previously the Rust server accepted only fixed-offset zones (`+05:30`,
`UTC`) and errored on a named zone, since it has no Python `zoneinfo` to defer to.

This is the unambiguous instant-to-wall-clock direction: a UTC instant maps to
exactly one local time in any zone, so the bundled `chrono-tz` database and Python
`zoneinfo` agree. (`$dateFromString`'s named-zone form — naive-local-to-instant,
which is ambiguous across a DST gap/overlap — still defers to the Python oracle.)

#### Added

- `secantus-core`: `chrono-tz` dependency and a named-zone offset resolver
  (`named_tz_offset_ms`) wired into `$dateToString`. A named zone resolves its
  DST-correct UTC offset at the rendered instant; an unknown zone name still
  defers. Both servers now agree on named-timezone `$dateToString`.

### Rust server: connection-auth mutex locks are poison-tolerant

The three production sites that lock the per-connection auth mutex in
`secantus-commands` used `.lock().expect("conn auth mutex poisoned")`, which
would panic if the mutex had been poisoned by an earlier panic while the lock
was held. They now use the same `.lock().unwrap_or_else(|e| e.into_inner())`
pattern as the rest of the crate (`logbuf`, `transactions`), recovering the
guard instead of panicking — so a poisoned auth mutex degrades to a caught
error at the `catch_unwind` dispatch boundary rather than compounding into a
second panic. No behaviour change on the healthy path. Found by the nightly
security review (2026-07-04 §I14).

#### Changed

- `secantus-commands` (`lib.rs` ×2, `auth.rs` ×1): connection-auth mutex locks
  are poison-tolerant. The two remaining `.lock().unwrap()` sites the review
  flagged are in `#[cfg(test)]` mock stores and are intentionally left panicking
  (a test that poisons its own mock lock should fail loudly).
### Rust server: `$dateToString` now formats dates in named IANA timezones

The Rust server can now render `$dateToString` in a named IANA timezone —
`{$dateToString: {date: "$d", timezone: "America/New_York"}}` — with the correct
daylight-saving offset for the instant being formatted, exactly as the Python
server (and real mongod) do. A summer date shifts by `-04:00` (EDT), a winter one
by `-05:00` (EST); `Europe/Dublin`, `Asia/Tokyo`, and every other zone resolve the
same way. Previously the Rust server accepted only fixed-offset zones (`+05:30`,
`UTC`) and errored on a named zone, since it has no Python `zoneinfo` to defer to.

This is the unambiguous instant-to-wall-clock direction: a UTC instant maps to
exactly one local time in any zone, so the bundled `chrono-tz` database and Python
`zoneinfo` agree. (`$dateFromString`'s named-zone form — naive-local-to-instant,
which is ambiguous across a DST gap/overlap — still defers to the Python oracle.)

#### Added

- `secantus-core`: `chrono-tz` dependency and a named-zone offset resolver
  (`named_tz_offset_ms`) wired into `$dateToString`. A named zone resolves its
  DST-correct UTC offset at the rendered instant; an unknown zone name still
  defers. Both servers now agree on named-timezone `$dateToString`.

### Projection inclusion/exclusion mix reports mongod's specific error code

Mixing an inclusion and an exclusion in the same `find()` projection (e.g.
`{a: 1, b: 0}`) now fails with mongod's *specific* per-field error instead of a
generic `TypeMismatch`. An exclusion inside an inclusion projection returns
`Location31254` ("Cannot do exclusion on field b in inclusion projection"); an
inclusion inside an exclusion projection returns `Location31253` ("Cannot do
inclusion on field b in exclusion projection"). The offending field is named in
the message, matching real mongod and the Rust server (which already emitted
these codes). Drivers' projection-error tests assert both the code and the exact
wording, so this closes a pymongo-gauge fidelity gap.

#### Fixed

- `projection.py`: `_detect_inclusion` validates field-by-field in order and
  raises `ProjectionError` with `code`/`code_name` pinned to `31254`/`31253`
  (the offending field named); `ProjectionError` gained a `code`/`code_name`
  constructor so the dispatch layer surfaces the specific code instead of `14`.
### The Rust server prunes its oplog from write volume alone

The standalone Rust `secantusdb` server used to prune its oplog only from
the noop-heartbeat thread — and that thread is off by default
(`--noop-heartbeat-seconds 0`). A long-lived, busy server with the
default configuration could therefore grow its oplog past the retention
window and entry cap without bound, since nothing was trimming it. The
Rust storage engine now self-prunes on the write path, exactly as the
Python server has always done: every 1000 emitted oplog entries it runs
an opportunistic `prune_oplog`, so the oplog stays bounded from writes
alone, with or without a heartbeat. Document data is never touched by a
prune.

#### Fixed
- `secantus_storage::emit_oplog` bumps an in-memory emit counter and runs
  an opportunistic `prune_oplog` every `OPLOG_PRUNE_INTERVAL` (1000)
  entries — mirroring the Python server's `_emit_oplog`. The prune reuses
  the write path's already-held storage lock (`prune_oplog_inner(...,
  take_lock: false)`) and is best-effort, so it never fails an otherwise
  successful write.

### `Storage` is now a context manager

`Storage` gained the `__enter__` / `__exit__` protocol, so you can write
`with Storage(path) as store:` and have WiredTiger torn down (background threads
joined, oplog meta persisted, connection closed) on block exit — even if the
body raises — instead of relying on the embedder to remember `close()`.
`close()` remains idempotent, so an explicit close inside the block is still
safe. Found by the nightly security review (2026-07-04 §I12).

#### Added

- `Storage.__enter__` / `Storage.__exit__` — context-manager support that calls
  `close()` on exit. Tests in `tests/test_storage_ctxmgr.py`.

### The standalone Rust server now honours every Python-server CLI flag

The pure-Rust `secantusdb` binary — the standalone server you run with
no Python interpreter in the process — used to accept only a subset of
the daemon flags the Python server understands. That gap meant a
`secantusdb.toml` or flag set tuned for one server wouldn't drive the
other. It's now closed: the Rust binary accepts the same eight
configuration flags the Python launcher does, with real behaviour behind
each, so a single configuration drives either server identically.

`--config` loads a `secantusdb.toml` with the same discovery order
(`./`, `~/.secantus/`, `/etc/secantus/`), the same `defaults < file <
CLI flag` precedence, table renames, and strict unknown-key/table
rejection as the Python loader. `--log-level` initialises a logger to
the requested level; `--cache-size`, `--session-max`, and
`--sync-on-commit` tune the WiredTiger connection; and
`--oplog-retention-seconds`, `--oplog-max-entries`, and
`--noop-heartbeat-seconds` control oplog retention plus a background
heartbeat that keeps quiet change-stream cursors' resume tokens inside
the retention window. The heartbeat thread and a TTL sweeper also give
the standalone binary the periodic oplog-prune and expired-document
maintenance the Python daemon has always run.

One behavioural alignment worth calling out: the standalone daemon's
default WiredTiger cache is now `1G`, matching `python -m secantus`
(the embedded Rust handle's default is unchanged).

#### Added
- The `--config` / `--log-level` / `--cache-size` / `--session-max` /
  `--sync-on-commit` / `--noop-heartbeat-seconds` /
  `--oplog-retention-seconds` / `--oplog-max-entries` flags on the
  standalone `secantusdb` binary, with a faithful TOML config loader
  (`secantus-server`'s new `config` module) mirroring
  `src/secantus/config.py`'s precedence and validation.
- Background noop-heartbeat (with opportunistic `prune_oplog`) and
  TTL-sweep threads in the `secantusdb` binary, sharing one
  `Arc<Storage>` and joining on a shutdown flag before WiredTiger closes.
- `secantus_storage::wt_config` builds the WiredTiger connection string
  from the resolved cache-size / session-max / sync-on-commit knobs.

#### Changed
- The standalone `secantusdb` daemon defaults its WiredTiger cache to
  `1G` (matching the Python server) instead of the engine's `256M`
  default.
### Projection inclusion/exclusion mix reports mongod's specific error code

Mixing an inclusion and an exclusion in the same `find()` projection (e.g.
`{a: 1, b: 0}`) now fails with mongod's *specific* per-field error instead of a
generic `TypeMismatch`. An exclusion inside an inclusion projection returns
`Location31254` ("Cannot do exclusion on field b in inclusion projection"); an
inclusion inside an exclusion projection returns `Location31253` ("Cannot do
inclusion on field b in exclusion projection"). The offending field is named in
the message, matching real mongod and the Rust server (which already emitted
these codes). Drivers' projection-error tests assert both the code and the exact
wording, so this closes a pymongo-gauge fidelity gap.

#### Fixed

- `projection.py`: `_detect_inclusion` validates field-by-field in order and
  raises `ProjectionError` with `code`/`code_name` pinned to `31254`/`31253`
  (the offending field named); `ProjectionError` gained a `code`/`code_name`
  constructor so the dispatch layer surfaces the specific code instead of `14`.

### Admin UI: stop passing the raw (credential-bearing) MongoDB URI into the server page

The admin server page's render context included `current_uri_raw`, the raw
`mongo_uri` (which can carry a username/password), even though no template ever
referenced it — a latent credential-exposure surface with no live leak. Removed
the dead context variable; the page continues to show the scrubbed
`current_uri_display`. Found by the nightly security review (2026-07-04 §I13).

#### Security

- `admin/routers/server.py`: drop the unused `current_uri_raw` template context
  variable so the unscrubbed connection URI is no longer handed to the renderer.

### PostgreSQL/SQL server: malformed messages get an error reply instead of a dropped connection

The PG/SQL server's simple-query loop now answers a malformed message with a
proper `ErrorResponse` rather than letting the exception escape to the outer
handler and silently drop the connection. Most notably, a **SQL syntax error**
over the simple-query protocol previously escaped (the parse ran outside the
handler's `try`) and dropped the connection with no reply; it now returns
`42601` and the connection stays alive. Invalid UTF-8 in a query message returns
`08P01` and the connection survives (the message was length-framed, so the byte
stream is still in sync), and a genuine framing error (an implausible length
prefix) sends a FATAL `08P01` before closing instead of dropping silently.

Found by the nightly security review (2026-07-04 §I16).

#### Fixed

- `sql/pgserver.py`: `planner.parse` moved inside `_handle_query`'s try so a
  syntax error returns `42601` instead of dropping the connection; `_query_loop`
  now catches `PGProtocolError` (framing) and `UnicodeDecodeError` (query text)
  and replies with `08P01`. Regression: `tests/test_pgserver_framing.py`.

### PostgreSQL/SQL server: a malformed SCRAM client-first is a typed auth error

A truncated SCRAM gs2 header (e.g. a client-first message of just `"n,"`, with no
bare message after the header) made `ScramExchange.server_first` raise a bare
`ValueError` from an unpack, caught only by the connection's outer generic
handler. It now raises the typed `PGAuthError`, consistent with the rest of the
PG-auth path — the connection still fails cleanly with no leak, just via the
right exception type. Found by the nightly security review (2026-07-04 §I21).

#### Fixed

- `sql/pgauth.py`: `ScramExchange.server_first` guards the gs2-header split and
  raises `PGAuthError` on a truncated header instead of an unpack `ValueError`.
  New unit tests in `tests/test_pgauth.py`.

### `$push` / `$addToSet` honour the `$each` modifier

`$push` and `$addToSet` now unwrap the `$each` modifier and append every element,
instead of storing the `{$each: […]}` document as a single array element. So
`{$push: {scores: {$each: [90, 85, 82]}}}` appends three scores, and
`{$addToSet: {tags: {$each: ["a", "b", "a"]}}}` adds each not-already-present tag —
matching MongoDB. `$push` also honours the companion modifiers `$position` (insert
at an index, negative from the end), `$slice` (keep the first N / last |N| / none),
and `$sort` (order the array — whole elements by `1`/`-1`, or documents by a
`{field: dir}` spec, in BSON order).

Previously both operators appended the modifier document verbatim — a silent
data-shape bug for one of the most common update forms. It ships on both the
Python and Rust servers, pinned by the update parity suite; the Rust engine
computes `$each` / `$position` / `$slice` natively and defers `$sort` (BSON-order
array sort) to the Python oracle.

#### Fixed

- `$push` / `$addToSet` `$each` is now unwrapped (multi-element append / add),
  with `$push` `$position` / `$slice` / `$sort` modifiers, instead of the `$each`
  document being stored as a single element. Ships on both servers
  (`update.apply_update` + `secantus-core::update`).

### Rust server: correct error codes for unrecognized / Atlas aggregation stages

The Rust server now validates aggregation stage names up-front, matching the
Python server (and mongod): an unrecognized stage (`{$badStage: …}`) is rejected
with `Location40324` ("Unrecognized pipeline stage name"), and an Atlas-only stage
(`$search` / `$vectorSearch` / `$searchMeta` / `$listSearchIndexes`) with
`CommandNotSupported` (115) and the Atlas configuration message. Previously both
surfaced as a generic `BadValue` (2), so drivers couldn't tell an unknown stage
from any other unsupported construct.

#### Fixed

- **Rust server:** an unrecognized aggregation stage now returns code 40324 (was
  2), and an Atlas-only stage returns 115 with the Atlas message
  (`aggregate::validate_stage_names`, recognized-stage set kept in sync with the
  Python `aggregate._STAGES` registry). Regression:
  `tests/test_rust_server_smoke.py::test_aggregate_stage_name_validation_against_rust_server`.

### PostgreSQL/SQL server: an unexpected internal error no longer leaks its Python text to the client

When a statement hit an *unexpected* exception (not a curated `SQLError`), the
PostgreSQL/SQL server sent the raw `str(exc)` back to the client as
`internal error: <text>`. That text could disclose internal file paths, type
names, or document values. The server now logs the full exception server-side
(as it already did) but answers the client with a generic `XX000 internal error`,
matching the Mongo dispatch's discipline of never leaking a Python traceback to
the wire. Curated `SQLError`s are unaffected — they still surface their real
SQLSTATE and user-facing message.

Found by the nightly security review (2026-07-04 §I17).

#### Security

- `sql/pgserver.py` (simple-query + COPY paths) and `sql/pgextended.py` (extended
  protocol) no longer interpolate the raw exception into the `ErrorResponse` for
  an unexpected internal error. Regression: `tests/test_pgserver_error_hygiene.py`.

### Reads on a view resolve its pipeline

`find`, `aggregate`, and `count_documents` on a view now run the view's stored
pipeline against its base collection instead of returning nothing. Create a view
with `db.createView("active", "users", [{$match: {status: "active"}}])` and a
`find` / `aggregate` on `active` reads exactly the matching users — with the
caller's filter, sort, skip, limit, and projection applied on top (a `find` is
translated into the equivalent aggregate over the base collection). A view defined
on another view resolves recursively.

Previously only the `count` command resolved views; `find` and `aggregate` (and
therefore `count_documents`, which pymongo implements via `aggregate`) treated the
view as an empty collection.

#### Fixed

- `find` / `aggregate` on a view now resolve the view's `viewOn` + pipeline
  (recursively for a view-on-a-view) via `commands._resolve_view`, applying the
  request's own filter/sort/skip/limit/projection over the result. Fixes
  `count_documents` on a view returning 0.

### Rust server: reads on a view resolve its pipeline

The Rust server now mirrors the Python server: `find`, `aggregate`, and
`count_documents` on a view resolve the view's `viewOn` + stored pipeline against
its base collection (recursively for a view-on-a-view) instead of returning
nothing. The aggregate command's initial fetch resolves the view chain (next to
the leading-`$match` / `$geoNear` lifts, keeping the view name for the reply `ns`);
`find` on a view is translated into the equivalent aggregate. Previously the Rust
server treated a view as an empty collection.

#### Fixed

- **Rust server:** `find` / `aggregate` / `count` on a view resolve
  `viewOn` + `viewPipeline` (`aggregate::resolve_view`), applying the request's own
  filter/sort/skip/limit/projection on top. Regression:
  `tests/test_rust_server_smoke.py::test_view_reads_resolve_against_rust_server`.

### Rust server: refuses direct writes to synthetic read-only views

The Rust server now rejects a direct `insert` / `update` / `delete` on
`local.oplog.rs` or `admin.system.users` with `Unauthorized` (13), matching the
Python server (and mongod's RBAC-denial code). These namespaces are synthetic
read-only views — `local.oplog.rs` projects the oplog WT table (written only via
oplog emission) and `admin.system.users` is fronted by `createUser` /
`updateUser` / `dropUser` — so a direct write would land in the wrong table or
break the view's invariants. Previously the Rust server silently accepted such
writes. A regular collection write is unaffected.

#### Fixed

- **Rust server:** direct `insert` / `update` / `delete` on `local.oplog.rs` /
  `admin.system.users` is now rejected with code 13 (matching the Python server),
  instead of being silently accepted. Regression: `tests/test_rust_server_smoke.py::
  test_synthetic_view_write_rejected_against_rust_server`.

### PostgreSQL/SQL server: per-statement RBAC, reusing the Mongo role model

The PostgreSQL/SQL server gained an authorization layer. Until now an
authenticated SQL client — or, in the documented trust default, any client —
could run any statement against any database it named at connect time, so a SQL
client had broader effective access than an equivalently-authenticated Mongo
client on the same data. The SQL surface now enforces the *same* RBAC engine the
Mongo server uses (`secantus.rbac`): each statement maps to one action on the
connection's database and is checked against the authenticated user's roles.

Authorization is opt-in and backward-compatible. Start the server with
`require_auth=True` and per-user role bindings via the new `user_roles` argument
(`{"analyst": [{"role": "read", "db": "shop"}]}`) to turn it on; without
`user_roles`, and for the embedded `run_sql` API, the surface stays unrestricted
exactly as before. Built-in roles (`read` / `readWrite` / `dbAdmin` / `dbOwner` /
`root`) resolve directly and custom roles resolve through the shared roles table,
so a role defined once governs both protocols on a shared `Storage`. A denied
statement returns SQLSTATE `42501` and the connection survives.

Found by the nightly security review (issue #193).

#### Added

- `SecantusPGServer(user_roles=...)`: per-user RBAC role bindings, enforced
  per-statement when `require_auth` is on. New pure `sql/authz.py` maps each
  statement to an `rbac` action and calls `rbac.check_privilege`; transaction
  control, `SET`/`SHOW`, and cursor navigation need no privilege. New
  `errors.insufficient_privilege` (SQLSTATE 42501).

#### Security

- SQL clients on a `Storage` shared with a Mongo server are now gated by the same
  roles as Mongo clients, closing the "authenticated SQL client has unrestricted
  access" gap. Regressions in `tests/test_sql_authz.py` (engine-level, built-in +
  custom roles) and `tests/test_pgserver_auth.py` (over the wire).

### Rust server: rejects a malformed `writeConcern`

The Rust server now validates a write command's `writeConcern` before running it,
matching the Python server (and mongod). A negative or too-large integer `w` is a
`FailedToParse` (9), a string `w` other than `"majority"` is an
`UnknownReplWriteConcern` (79), and a bool / non-number-or-string `w` — or a
non-bool/int `j` or non-number `wtimeout` — is a `TypeMismatch` (14). A
well-formed writeConcern is accepted as before, and a satisfiable-but-too-wide
`w > 1` still succeeds with the single-node `writeConcernError` attached. The check
runs in `dispatch` for every write command (insert / update / delete /
findAndModify / create / collMod / createIndexes / drop / dropIndexes /
dropDatabase / renameCollection).

#### Fixed

- **Rust server:** malformed `writeConcern` values are now rejected with mongod's
  codes (9 / 79 / 14) before the write runs, instead of being silently accepted.
  Regression: `tests/test_rust_server_smoke.py::
  test_write_concern_validation_against_rust_server`.

### Rust server: `$lookup` drives the foreign index (and fixes its result order)

The Rust server's simple-form `$lookup` now drives a per-outer-doc index probe
when the foreign collection has a leading-field index on `foreignField`
(single-field, compound-prefix, or multikey — all resolve to an IXSCAN via
`Storage::find`), falling back to the full-scan hash-join only when no such index
exists. This mirrors the Python server's `$lookup` path.

Besides the performance win, this **fixes a two-server divergence**: the previous
hash-join returned the joined `as` array in foreign-collection scan order, while
the Python server (and the index probe) return it in index order. For an indexed
`$lookup` the two servers now produce byte-for-byte identical results, including
the order of documents within `as`. A regression test drives the Rust server and
asserts the index-ordered output.

#### Fixed

- **Rust server:** simple-form `$lookup` over an indexed `foreignField` now returns
  the `as` array in index order (matching the Python server) instead of
  foreign-scan order, and rides the index instead of materialising the whole
  foreign collection. Regression: `tests/test_rust_server_smoke.py::
  test_lookup_index_order_against_rust_server`.

### PostgreSQL/SQL server: connection cap, cursor caps, and a parser guard

The PostgreSQL wire server gained the resource limits its MongoDB counterpart
already had, closing a compounding denial-of-service surface (all reachable
pre-auth, since `require_auth` defaults off):

- **Connection cap.** `SecantusPGServer` now enforces `max_connections` (default
  1000, matching the Mongo server's `DEFAULT_MAX_CONNECTIONS`). An over-cap
  accept is closed immediately instead of spawning a handler thread.
- **Per-session cursor caps.** `DECLARE CURSOR` (which eagerly materializes its
  whole result set) now rejects a session's 101st open cursor, and a cursor
  whose result exceeds 1,000,000 rows, with `program_limit_exceeded` (SQLSTATE
  54000). Combined with the connection cap, total cursor memory is bounded.
- **Parser guard.** `planner.parse` rejects a statement longer than 1 MB before
  handing it to sqlglot, and converts the `RecursionError` from a deeply-nested
  statement (e.g. hundreds of parentheses — a ~600-byte trigger) into a clean
  54000 error instead of relying on the connection loop's broad `except`.

Found by the nightly security review (issue #194).

#### Security

- `sql/pgserver.py`: `max_connections` cap (default `DEFAULT_MAX_CONNECTIONS`).
- `sql/engine.py`: `MAX_CURSORS_PER_SESSION` (100) + `MAX_CURSOR_ROWS` (1e6) caps
  on `DECLARE CURSOR`.
- `sql/planner.py`: `MAX_SQL_LENGTH` (1 MB) cap + `RecursionError` guard in
  `parse`. New `errors.program_limit_exceeded` (SQLSTATE 54000). Regressions in
  `tests/test_sql_cursors.py` + `tests/test_pgserver.py`.

### `$geoNear` rides the geo index instead of scanning

A bounded `$geoNear` — one with a `maxDistance` — no longer walks the whole
collection computing a distance for every document. When it's the leading stage
and the collection has a matching geo index on the queried field, SecantusDB now
lifts the search into a conservative `$geoWithin` candidate query and serves it
through the same geo-index path `$near` and `$geoWithin` already use, then
computes exact distances and sorts over just the candidates.

The candidate radius is inflated by a negligible epsilon so the fetched set is a
strict superset of the exact within-`maxDistance` set; the `$geoNear` stage then
re-applies the exact distance filter, so results — the documents, their order, and
the attached `distanceField` — are byte-for-byte identical to the brute-force
path. Only the number of documents fetched shrinks. An unbounded `$geoNear` (no
`maxDistance`) must still return every document in distance order, so it keeps
scanning; the optimization is scoped to the bounded case, and a mismatched index
type falls back to the full scan. A randomized regression test asserts the
optimized output equals the scan output across many queries.

#### Changed

- `$geoNear` with a `maxDistance` and a matching `2dsphere` / `2d` index now
  fetches candidates through the geo index (`aggregate._geo_near_index_filter`,
  lifted into the aggregate command's initial fetch) instead of a full collection
  scan. Output is unchanged. (Rust-server mirror below.)

### Rust server: `$geoNear` rides the geo index for a bounded search

The Rust server now mirrors the Python server's `$geoNear` index optimization. A
leading `$geoNear` with a `maxDistance` and a matching `2d` / `2dsphere` index no
longer scans the whole collection: the search is lifted into a conservative
`$geoWithin` candidate fetch in the aggregate command's initial fetch (next to the
existing leading-`$match` lift), and the `$geoNear` stage then re-applies the exact
distance filter over just the candidates.

The candidate radius is inflated by a negligible epsilon so the fetched set is a
strict superset of the exact within-`maxDistance` set, keeping the output — docs,
order, and attached `distanceField` — byte-for-byte identical to the brute-force
path. An unbounded `$geoNear` still scans, and a mismatched index type falls back
to the full scan. A regression test drives the Rust server directly, asserting the
optimized output equals the scan output across many random queries.

#### Changed

- **Rust server:** `$geoNear` with a `maxDistance` and a matching geo index fetches
  candidates through the index (`aggregate::geo_near_index_filter` in
  `secantus-commands`) instead of a full collection scan. Output is unchanged.
  Regression: `tests/test_rust_server_smoke.py::
  test_geo_near_index_optimization_against_rust_server`.

### Constant-time secret comparison in the PostgreSQL SCRAM + admin-token checks

Two authentication comparisons used a plain `!=` / `==` on secret material,
which CPython does not guarantee to run in constant time — a timing side-channel
that can narrow a secret a byte at a time. The PostgreSQL/SQL server's SCRAM
proof check (`sql/pgauth.py`) compared the recomputed stored-key digest with
`!=`, and the admin console's token middleware (`admin/middleware.py`) compared
the presented token with `!=` / `==` on both the HTTP and WebSocket paths. All
three now use `hmac.compare_digest` (bytes-encoded, so a non-ASCII presented
value is rejected rather than raising), matching the Mongo-side SCRAM check in
`secantus.auth` which was already constant-time. Behaviour is otherwise
unchanged — valid credentials/tokens are accepted, wrong or missing ones
rejected. Found by the nightly security review (issue #195).

#### Security

- `sql/pgauth.py`: SCRAM stored-key comparison uses `hmac.compare_digest`.
- `admin/middleware.py`: HTTP `TokenAuthMiddleware` and `verify_websocket_token`
  compare the admin token with `hmac.compare_digest` (and reject a
  missing/non-ASCII token without raising). Regression:
  `tests/test_admin_skeleton.py::test_verify_websocket_token_is_constant_time_and_robust`.

### Rust server: bool-as-int range comparison in the query matcher

The Rust server's query matcher now compares a boolean field against a numeric
`$gt` / `$lt` / `$gte` / `$lte` bound (and vice versa) natively, instead of
deferring the whole match to a `BadValue`. Following the Python oracle — which
compares with Python's `<`, where `bool` is an `int` subclass — `True` counts as
`1` and `False` as `0`: `{x: {$gt: 0}}` now matches `x: true` on the Rust server,
matching the Python one. A bool compared against a non-numeric type (string,
Decimal128, date, …) is `TypeError` in Python, i.e. no match, and is reproduced
as such. Boolean *equality* is unaffected (a bool stays distinct from `1`).

Pinned by the query parity suite (curated bool-vs-number / bool-vs-non-numeric /
multikey cases, plus the existing randomized fuzz whose scalar corpus already
includes booleans — 0 divergences).

#### Added

- **Rust server:** bool-as-int `$gt` / `$lt` / `$gte` / `$lte` comparison in the
  `secantus-core` query matcher (numeric compare vs int / long / double; no match
  vs any other type), matching the Python oracle's `<` semantics.

### Rust server: `getLog` returns a real in-memory log

The Rust server's `getLog` was a stub returning an empty array; it now surfaces a
real bounded in-memory log ring buffer (`secantus_commands::logbuf::LogBuffer`,
5000-entry cap, the Rust port of `logbuf.py`). The server records a startup line
and a `"connection accepted"` NETWORK line per connection, and `getLog: "global"`
returns them as mongod-shaped pre-formatted strings
(`"<ts> <level> <component> <msg>"`) with `totalLinesWritten`. The admin
console's Logs page now shows activity against a Rust target instead of an empty
table. Slice 5 of the Rust admin-command parity work (issue #163).

This resolves the last genuine gap in #163: with `getLog` real, `killOp` and the
role grant/revoke + native backup/restore/prune commands all landed, and the
`profile` command was already at parity with the Python server (both persist the
level / slowms / sampleRate config; neither captures slow ops — out of scope for
the surrogate). `currentOp` / `serverStatus` remain intentionally minimal.

#### Added

- **Rust server:** `secantus_commands::logbuf::LogBuffer` + a `getLog`-backed log
  ring buffer wired through the server (connection-accept + startup lines) and a
  `logs` handle on `CommandContext`. Regressions: `crates/secantus-commands` unit
  tests (buffer append/tail/capacity; `getLog` formatting; empty without a
  buffer) and `tests/test_rust_server_smoke.py::test_get_log_returns_connection_lines`.

### Rust server: `$min` / `$max` / `$addToSet` / `$pull` update operators

The Rust server now applies four more update operators natively instead of
rejecting them: `$min` and `$max` (keep the smaller / larger of the current and
given value), `$addToSet` (append to an array only if not already present), and
`$pull` (remove the array elements equal to a value). Previously each surfaced as
a `BadValue` error on the Rust server; they now match the Python server.

Fidelity follows the Python oracle exactly. `$min`/`$max` compare with Python's
`<` semantics for numeric / string / date pairs (bool counts as its integer
value; a cross-type comparison Python would raise on defers to Python), and an
absent-or-null field is treated as "no current value". `$addToSet` and `$pull`
use Python `==` element equality — cross-type-equal numerics (`1` == `True`) and
structural document/array equality included — via the shared value-equality
helper. It's pinned by the update parity suite with curated edge cases and a fuzz
corpus (0 divergences). `$bit` was already native.

#### Added

- **Rust server:** `$min` / `$max` / `$addToSet` / `$pull` update operators in the
  `secantus-core` update engine, matching the Python oracle's comparison / `==`
  element semantics.

### Rust server: `$dateFromString` `format` (strptime)

The Rust server now parses a `$dateFromString` `format` string natively for the
numeric-directive subset (`%Y` `%y` `%m` `%d` `%H` `%M` `%S` `%j` `%%`, literals,
and whitespace), instead of the whole expression being unsupported. So
`$dateFromString: {dateString: "15/01/2024", format: "%d/%m/%Y"}` now evaluates on
the Rust server, matching the Python one.

Fidelity is structural: the format is translated into a regex built from
CPython's own `_strptime` per-directive sub-patterns, so field matching — the
2-digit-year pivot (`00`–`68` → 2000s, `69`–`99` → 1900s), the value-range digit
rules, single-digit leniency, day-of-year, and full-input consumption — is
identical to Python's by construction. It's pinned by the expression parity suite
with a 6000-case fuzz corpus (0 divergences). Directives outside the subset
(`%z` / `%Z` / `%a` / `%b` / `%p` / …), a `%j` combined with `%m`/`%d`, a leap
second, or any input Python would reject still defer to the Python oracle.

#### Added

- **Rust server:** `$dateFromString` `format` (strptime) for the numeric-directive
  subset, built from CPython `_strptime`'s exact regex fragments for byte-faithful
  field matching. Combined with the fixed-offset `timezone` support, a naive
  strptime result is interpreted in the given offset zone.

### Rust server: `killOp`

The Rust server now implements `killOp`. Real mongod signals a per-op interrupt
flag; SecantusDB's faithful analogue (in both servers) is "close the socket" —
since we model one in-flight op per connection, the `op` a caller passes is the
connection's `conn_id` (readable off `hello`'s `connectionId`). The server gained
a live-connection registry (`conn_id → socket clone`, populated on accept and
cleared on disconnect); `killOp` shuts down the target socket, so the connection
thread's next read returns 0 and the connection ends. The opid is accepted as
Int32 / Int64 / integral Double / numeric string, and the reply mirrors the
Python handler: `{info: "operation killed" | "no operation with that opid" |
"no connection registry", ok: 1}`, or `TypeMismatch` for a non-integer `op`.
The admin console's Connections → Kill button now works against a Rust target.
Slice 4 of the Rust admin-command parity work (issue #163).

Remaining #163 gaps: fleshing out the `getLog` / `profile` stubs (both need
real capture infrastructure — a log ring buffer and slow-op timing).

#### Added

- **Rust server:** `killOp` (gated by `A_KILLOP` under `--auth`), backed by a new
  server-side connection registry + a `ConnectionKiller` handle on
  `CommandContext`. Regressions: `crates/secantus-commands` unit tests
  (registry-present/absent, found/not-found, numeric-string opid, non-integer →
  `TypeMismatch`) and
  `tests/test_rust_server_smoke.py::test_kill_op_closes_a_connection`.

### Rust server: fixed-offset `timezone` on `$dateToString` / `$dateFromString`

The Rust server now handles a fixed-offset `timezone` on the `$dateToString` and
`$dateFromString` aggregation-expression operators natively, instead of the whole
expression being unsupported. `$dateToString` with `timezone: "+05:30"` shifts the
wall clock before formatting; `$dateFromString` with a `timezone` interprets a
naive date string as being in that zone (and ignores it when the string already
carries its own offset), matching the Python server exactly. The `UTC` / `GMT`
aliases and both the `±HHMM` and `±HH:MM` offset spellings are accepted.

Named IANA zones (`America/New_York`) still defer — they need a bundled tz
database — as does a `$dateFromString` `format` (strptime). This narrows the gap
between the two servers on the date operators; the behaviour is pinned against the
Python oracle by the expression parity suite.

#### Added

- **Rust server:** fixed-offset / `UTC` / `GMT` `timezone` support on
  `$dateToString` and `$dateFromString` (offset arithmetic in the shared
  `secantus-core` expression engine). Named IANA zones and `format` strptime still
  defer.

### `$jsonSchema` gains `uniqueItems`

The `$jsonSchema` query operator now understands `uniqueItems`. With
`uniqueItems: true` on an array schema, a document matches only when every element
of the array is distinct — the way to say "this list has no repeats" in a
validator or a `$jsonSchema` query. Elements are compared by value: cross-type
numerics that are numerically equal (`1` and `1.0`) count as duplicates, and
documents compare field-by-field in order, matching how mongod compares them.

The check ships on both the Python and Rust servers, pinned by the query parity
suite; it reuses the shared byte-sortable value encoding as the equality key, so
both engines agree bit-for-bit. (One documented gap: cross-type-equal numerics
*nested inside* a document element — `[{a: 1}, {a: 1.0}]` — are treated as
distinct on both servers; top-level scalar arrays are fully faithful.)

#### Added

- `$jsonSchema` `uniqueItems: true` — rejects arrays with duplicate elements
  (value equality, cross-type-numeric aware for top-level scalars). `false` is a
  no-op.

### Rust server: `grantRolesToUser` / `revokeRolesFromUser`

The Rust server now implements the two user-role management commands it was
missing: `grantRolesToUser` adds roles to a user's assignment list (deduped by
`(role, db)`), and `revokeRolesFromUser` removes them. Both validate the
requested roles against the built-in + custom role catalogue (`RoleNotFound`),
require the user to exist (`UserNotFound`), and refresh the calling connection's
effective roles so a privilege change takes effect immediately — matching the
Python handlers. Previously the Rust server could only set a user's roles at
`createUser` / `updateUser` time; the admin console's Users → Roles editor can
now grant/revoke against a Rust target. Slice 3 of the Rust admin-command parity
work (issue #163).

Remaining #163 gaps: `killOp`, and fleshing out the `getLog` / `profile` stubs.

#### Added

- **Rust server:** `grantRolesToUser` / `revokeRolesFromUser` commands (gated by
  `A_GRANT_ROLE` / `A_REVOKE_ROLE` under `--auth`). Regressions:
  `crates/secantus-commands` unit tests (grant/revoke, dedup, `UserNotFound`,
  `RoleNotFound`) and
  `tests/test_rust_server_smoke.py::test_secantus_grant_revoke_roles_to_user`.

### Rust server: `secantusAdmin.restoreArchive`

The Rust server now implements `secantusAdmin.restoreArchive`, completing the
proprietary backup/restore + prune command family (`backupArchive` and the
prune commands already shipped). It extracts a backup `.tar.gz` (produced by
`backupArchive`) into a fresh `targetDir` the operator then points a new server
at — the running server's storage is untouched, the same side-channel restore
model as the Python command and real mongod's "stop, swap dbpath, start". It
rejects a non-empty target unless `allowExisting: true`, verifies the archive is
a genuine SecantusDB / WiredTiger backup *before* extracting (so a malformed
archive can't pollute the target), and returns `{targetDir, fileCount, archive,
ok}`, mirroring the Python handler. This is slice 2 of the Rust admin-command
parity work (issue #163); the admin console's Backup page can now drive native
archive restore against a Rust target.

Remaining #163 gaps: the standard admin commands `grantRolesToUser` /
`revokeRolesFromUser` / `killOp`, plus fleshing out the `getLog` / `profile`
stubs.

#### Added

- **Rust server:** `secantusAdmin.restoreArchive` wire command
  (`Storage::restore_archive` on the command trait →
  `secantus_storage::extract_backup_archive_ex`, with the target-emptiness /
  WiredTiger-metadata guards and abs-path + file-count reply). Regressions:
  `tests/test_rust_server_smoke.py::test_secantus_admin_restore_archive_roundtrips`
  and `::test_restore_archive_rejects_nonempty_target`.

### Rust server: `secantusAdmin.pruneOplog` and `pruneTtl` maintenance commands

The Rust server now implements two of the proprietary `secantusAdmin.*`
maintenance commands the Python server already had: `pruneOplog` (force an
immediate oplog-retention sweep) and `pruneTtl` (run TTL pruning across every
collection now). Both return `{pruned: <count>, ok: 1}`, mirroring the Python
handlers. The underlying storage already prunes on its own cadence; these
wire commands let an operator — or the admin console's Maintenance page —
drive a deterministic pass on demand. This closes the first slice of the Rust
admin-command parity gap (issue #163); the admin UI's capability probe will now
enable the "Prune oplog / TTL" buttons against a Rust target.

Remaining #163 gaps (tracked, not in this slice): `secantusAdmin.restoreArchive`
and the standard admin commands `grantRolesToUser` / `revokeRolesFromUser` /
`killOp`, plus fleshing out the `getLog` / `profile` stubs.

#### Added

- **Rust server:** `secantusAdmin.pruneOplog` / `secantusAdmin.pruneTtl` wire
  commands (`Storage::prune_oplog` / `prune_ttl_all` on the command `Storage`
  trait, forwarded by the WT adapter to the real storage engine). Regression:
  `tests/test_rust_server_smoke.py::test_secantus_admin_prune_commands`.

### `$derivative` and `$integral` gain a time `unit`

The `$setWindowFields` rate operators `$derivative` and `$integral` now accept a
time `unit`. Over a date-valued `sortBy`, `$derivative: {input: "$v", unit:
"hour"}` reports the change in `v` *per hour* — the x-axis is the date's epoch
milliseconds scaled into the requested unit — and `$integral` computes the
trapezoidal area with the same unit-scaled x-axis. This is how you express a rate
of change in meaningful units (per second, per hour, per day) instead of the raw
millisecond spacing between timestamps.

As with time-`unit` range windows, the `unit` requires a date `sortBy` (a numeric
sort with a `unit` is rejected) and the fixed-duration units
`week`/`day`/`hour`/`minute`/`second`/`millisecond` are supported; variable-length
`month`/`quarter`/`year` defer. The feature ships on both servers, pinned by the
`$setWindowFields` parity suite — the millisecond-to-unit scaling runs in IEEE
double on both sides so the results match bit-for-bit. This completes the
`$setWindowFields` time-`unit` surface.

#### Added

- `$setWindowFields` `$derivative` / `$integral` accept a fixed-duration time
  `unit` over a date `sortBy`, scaling the x-axis into that unit so the rate /
  area is expressed per unit. `unit` requires a date sortBy; variable-length
  units defer.

### `$setWindowFields` range windows over a date sortBy

A `$setWindowFields` value-`range` window can now span a *time* interval. Give the
window a `unit` — `week`, `day`, `hour`, `minute`, `second`, or `millisecond` —
and the numeric `range: [lower, upper]` bounds are measured in that unit against a
date-valued `sortBy` field. A `range: [-2, 0], unit: "day"` window is the trailing
three-day span ending at each row, so `{$sum: "$v"}` over it is a rolling 3-day
total regardless of how the dates are spaced.

The rule mongod enforces holds both ways: a `unit` requires a date `sortBy` (a
numeric sort with a `unit` is rejected), and a date `sortBy` in a range window
requires a `unit` (there is no implicit millisecond arithmetic on dates).
Variable-length units — `month`, `quarter`, `year` — are still rejected, since
their span depends on the calendar position. The feature ships on both the Python
and Rust servers, pinned by the `$setWindowFields` parity suite; the date x-axis
is carried as epoch milliseconds so both engines compute identical window bounds.

#### Added

- `$setWindowFields` `range` windows accept a fixed-duration time `unit`
  (`week`/`day`/`hour`/`minute`/`second`/`millisecond`) over a date `sortBy`,
  offsetting the bounds in that unit against the date's epoch millis. `unit` and a
  date sortBy are mutually required; variable-length `month`/`quarter`/`year`
  defer with an error.

### `$jsonSchema` gains `patternProperties` and `dependencies`

The `$jsonSchema` query operator now understands `patternProperties` and
`dependencies`. `patternProperties` applies a sub-schema to every field whose
*name* matches a regular expression — the way to say "every `s_*` field must be a
string" without listing them — and it also tells `additionalProperties: false`
which keys are legitimately covered, so pattern-matched fields are no longer
flagged as unexpected. `dependencies` expresses conditional structure: when a
trigger field is present, either a list of other fields must also be present
(property dependency) or the whole document must satisfy a sub-schema (schema
dependency) — e.g. "if `card` is set, `billing` is required."

Both ship on the Python and Rust servers, pinned by the query parity suite;
`patternProperties` reuses the shared regex engine.

#### Added

- `$jsonSchema` `patternProperties` (regex-keyed sub-schemas, also honoured by
  `additionalProperties`) and `dependencies` (property-list and schema forms).

### `$setWindowFields` completes its time-series window operators with `$derivative` and `$integral`

`$setWindowFields` now supports `$derivative` and `$integral`, the last two of
MongoDB's time-series window operators. Over the window, `$derivative` reports the
rate of change — the slope between the first and last points, `(yₙ − y₀) / (xₙ −
x₀)`, with the `sortBy` value as x and `input` as y — and `$integral` reports the
area under the curve by the trapezoidal rule. Together with `$shift`,
`$expMovingAvg`, `$locf`, and `$linearFill`, that rounds out the full set of
time-series window functions: rates, running smoothing, lag/lead, and gap-filling
all now run in-process.

Both operate over any window (`documents` or `range`), require a single ascending
numeric `sortBy` as the x-axis, and run the same IEEE-double arithmetic on the
Python and Rust servers (pinned bit-for-bit by the aggregation parity suite). A
`$derivative` window with fewer than two points is `null`. A time `unit` (for a
date x-axis) is not yet modelled and raises a clear error.

#### Added

- `$setWindowFields` `$derivative` and `$integral` window operators (`{input}`) —
  slope / trapezoidal area over the sortBy x-axis, over any window.

### `$setWindowFields` gains `$locf` and `$linearFill`

`$setWindowFields` now supports the two gap-filling window operators, `$locf` and
`$linearFill`. `{$locf: <expr>}` ("last observation carried forward") replaces a
null with the most recent non-null value seen in sort order — the standard way to
hold a reading steady until the next sample. `{$linearFill: <expr>}` instead draws
a straight line between the surrounding non-null anchors and reads off the missing
values along it, using the `sortBy` value as the x-axis. Leading nulls (for
`$locf`) and leading/trailing nulls (for `$linearFill`, which has nothing to
interpolate between) stay null.

Both run on the Python and Rust servers alike, pinned by the aggregation parity
suite (the interpolation is IEEE-double, so the two agree bit-for-bit). With these
plus `$shift` and `$expMovingAvg`, only `$derivative` and `$integral` remain of
the time-series window operators.

#### Added

- `$setWindowFields` `$locf` and `$linearFill` gap-fill window operators
  (`<expr>`) — prefix/partition-based, require a `sortBy` (`$linearFill` a single
  ascending numeric one).

### Rust server: an interior-NUL db/collection/index name no longer panics the connection

A well-formed BSON command whose database, collection, or index name carried
an embedded NUL byte (BSON strings are length-prefixed and may legally contain
one) reached the Rust server's WiredTiger key encoder (`secantus-wt`'s `cstr`,
`CString::new(..).expect(..)`) and **panicked**. Because the storage layer
serialises WiredTiger operations under a `std::sync::Mutex`, that panic unwound
while the lock was held and **poisoned it for every connection** — turning a
single crafted command into a whole-server denial of service, and dropping the
offending socket with no `{ok: 0, ...}` reply (unlike the Python server, which
catches every error in `dispatch`).

The Rust server now rejects an interior-NUL database / collection / index name
during command validation — before it reaches storage — with the same
`InvalidNamespace` error mongod returns, so the connection survives with a
clean wire reply. As defense-in-depth, the per-connection dispatch call is now
wrapped in `catch_unwind`: any future unguarded panic in a handler produces a
wire-level `InternalError` reply instead of a silent disconnect. This is the
Rust-side analogue of the earlier Python-server OP_QUERY hardening. Found by
the nightly security review (issue #139).

#### Security

- **Rust server:** interior-NUL `db` / `coll` / `index` names are rejected with
  `InvalidNamespace` before reaching the WiredTiger key encoder (which would
  otherwise panic and poison the shared storage mutex — a whole-server DoS).
  Per-connection dispatch is wrapped in `catch_unwind` so any residual panic
  surfaces as an `InternalError` wire reply rather than a dropped socket.
  Regressions in `crates/secantus-commands` unit tests.

### `admin.system.users` no longer leaks SCRAM credentials via find/count/aggregate

A query against `admin.system.users` — reachable through ordinary
`find` / `count` / `aggregate` / `explain` / `distinct` / `mapReduce`,
which need only the standard collection-read action — used to return each
user record *including* its SCRAM `credentials` blob (`storedKey`,
`serverKey`, `salt`, iteration count). That's the sensitive artifact:
even without the plaintext password it enables offline dictionary/brute
attacks (salt + iterations are the `/etc/shadow` equivalent) and, via
`serverKey`, lets an attacker stand up a rogue server that completes the
SASL server side. A principal holding only `read` / `readAnyDatabase` on
`admin` — a routine monitoring/backup grant — could read every user's
credential material, which is exactly what `usersInfo` intentionally
gates behind `A_VIEW_USER` + `showCredentials`.

The generic read path now strips `credentials` unconditionally, *before*
the filter runs, so it is never returned and can't be used as a
match-oracle either. Credentials remain reachable only through
`usersInfo` with `showCredentials` and the `A_VIEW_USER` privilege — the
one intentionally-gated path, unchanged. Found by the nightly security
review (issue #167).

#### Security

- `Storage._find_system_users` / `_count_system_users`: the SCRAM
  `credentials` blob is stripped from the generic `admin.system.users`
  read path (`find` / `count` / `aggregate` / …) before filtering, so a
  low-privilege reader can no longer harvest credential material.
  `usersInfo` (gated by `A_VIEW_USER` + `showCredentials`) is unchanged.
  Regressions in `tests/test_system_users_view.py`.
### `$setWindowFields` gains `$expMovingAvg`

`$setWindowFields` now supports `$expMovingAvg`, the exponential moving average —
the second time-series window operator (after `$shift`).
`{$expMovingAvg: {input: <expr>, N: <n>}}` smooths a series over the sorted
partition, weighting recent rows more heavily: each output is
`input·α + previous·(1−α)`, with `α = 2/(N+1)`. An explicit
`{alpha: <0…1>}` may be given instead of `N`. It's the standard smoothing for
noisy time-series — a trend line that reacts faster than a flat rolling average.

The recurrence runs in IEEE double on both the Python and Rust servers, so the
two agree bit-for-bit (pinned by the aggregation parity suite). The remaining
time-series operators (`$derivative`, `$integral`, `$linearFill`, `$locf`) still
raise a clear error.

#### Added

- `$setWindowFields` `$expMovingAvg` window operator (`{input, N | alpha}`) —
  prefix-accumulated per partition, requires a `sortBy`.

### `$jsonSchema` gains logical combinators and `additionalProperties`

The `$jsonSchema` query operator now understands the JSON-Schema logical
combinators — `allOf`, `anyOf`, `oneOf`, and `not` — plus `additionalProperties`.
`allOf`/`anyOf`/`oneOf` compose sub-schemas (all / at-least-one / exactly-one
must hold), `not` inverts one, and `additionalProperties` controls fields not
named in `properties`: `false` forbids them outright, while a sub-schema
validates each one. Together they cover the structural side of collection
validators that go beyond flat field constraints — "exactly one of these shapes",
"none of that", "no unexpected fields".

These keywords were previously absent from *both* servers; they now ship on the
Python and Rust servers alike, pinned by the query parity suite.
(`patternProperties` is still not modelled.)

#### Added

- `$jsonSchema` `allOf` / `anyOf` / `oneOf` / `not` combinators and
  `additionalProperties` (`true` / `false` / sub-schema).

### `$setWindowFields` gains the `$shift` window operator

`$setWindowFields` now supports `$shift` — the first of MongoDB's time-series
window operators. `$shift` reaches across the sorted partition to read a value
from a neighbouring row: `{$shift: {output: <expr>, by: <n>, default: <expr>}}`
evaluates `output` on the row `n` positions away (negative looks back, positive
looks ahead), falling to `default` (or `null`) when that position is past the
partition edge. It's the idiomatic way to compute a delta from the previous row,
peek at the next value, or lag/lead a series — without a self-join. Shifts never
cross a partition boundary.

The same semantics ship in both the Python and Rust servers, pinned by the
aggregation parity suite. The remaining time-series operators (`$derivative`,
`$integral`, `$expMovingAvg`, …) still raise a clear error.

#### Added

- `$setWindowFields` `$shift` window operator (`{output, by, default?}`) —
  position-based, per-partition, requires a `sortBy`.

### `$setWindowFields` learns value-based (range) windows

The `$setWindowFields` aggregation stage now supports value-based windows —
`window: {range: [lower, upper]}` — alongside the position-based `documents`
windows it already had. Where a `documents` window counts rows relative to the
current one, a `range` window is defined by the sortBy *value*: it includes every
row whose value falls in `[current + lower, current + upper]`. That's the natural
way to express "sum everything within 10 units of this point" or a gap-aware
running total, and it's what analytics pipelines reach for. Bounds may be a
number, `"current"` (this row's value), or `"unbounded"`.

The window resolves against a single ascending numeric sortBy field; a range
window with a time `unit`, or over a descending / multi-field / non-numeric sort,
still raises a clear error rather than guessing. The same semantics ship in both
the Python and Rust servers, pinned together by the aggregation parity suite.

#### Added

- `$setWindowFields` value-based windows (`window: {range: [lo, hi]}`) over a
  single ascending numeric `sortBy`, with `"unbounded"` / `"current"` / numeric
  bounds. Time-unit ranges and non-ascending / multi-field / non-numeric sorts
  remain deferred with a clear `AggregateError`.

### Smaller on-disk footprint: WiredTiger log pre-allocation disabled

Each on-disk SecantusDB instance used to reserve ~30 MB of WiredTiger log
files regardless of how little data it held — the active `WiredTigerLog`
plus two 10 MB `WiredTigerPreplog` files WT pre-allocates ahead of the
active log. That pre-allocation is a write-latency optimisation for
long-running, high-throughput servers; SecantusDB's instances are small,
ephemeral, in-process test databases, so it bought nothing and cost disk —
acutely on CI, where a full ~2000-test on-disk run retained thousands of
instances and exhausted the Windows runner's disk (`No space left on
device` → `WT_PANIC`). Disabling `log=(prealloc=false)` drops each
instance's log footprint from ~30 MB to ~10 MB with no durability change
(recovery replays the same log records; WT just allocates each segment on
demand). `file_max` stays 10 MB so a near-`maxBsonObjectSize` document
still fits in one segment.

#### Changed
- `Storage`: on-disk WiredTiger opens with `log=(...,prealloc=false)` —
  ~3x smaller per-instance log footprint, no durability impact.


### A malformed OP_QUERY frame returns BadValue instead of dropping the connection

A legacy `OP_QUERY` frame whose `fullCollectionName` carried no NUL
terminator made `bytes.index(b"\x00", ...)` raise an uncaught `ValueError`
that escaped the wire layer's `(InvalidBSON, _BodyBoundsError)` handler,
killed the connection handler without sending a reply, and logged a Python
traceback. The same gap existed for several siblings on the same
attacker-controlled path: a non-UTF-8 collection name (`UnicodeDecodeError`),
a frame truncated before the skip/return/query fields (`struct.error`), and a
negative/oversized declared query-doc length (the `OP_MSG` path already
guarded this with `_check_doc_len`, but `OP_QUERY` did not). The OP_MSG
kind-1 section-identifier `.index()` / `.decode()` had the identical
unterminated-cstring gap.

`_parse_op_query` is now hardened the same way `_parse_op_msg` already was —
every read on the network buffer raises `_BodyBoundsError` / `struct.error`,
which `read_message` translates into a `BadValue` (2) wire reply while keeping
the connection alive (matching `mongod`). `read_message` also now catches
`struct.error` as a backstop so no malformed frame can escape as an uncaught
exception. Found by the nightly security review (issue #116).

#### Security
- `wire._parse_op_query` / `_parse_op_msg`: malformed OP_QUERY/OP_MSG frames
  (missing cstring NUL, invalid UTF-8, truncation, bad BSON length) now yield
  a `BadValue` reply and a surviving connection instead of a dropped socket +
  logged traceback. Regression: `tests/test_wire_malformed.py` (5 new
  OP_QUERY cases).

### Admin UI security hardening (two CVEs + a stored-XSS fix)

Three findings from the nightly security review, all confined to the
optional `[admin]` extra (the loopback FastAPI console):

- **CVE-2026-48710 "BadHost" (issue #114):** the admin token middleware
  gated its `/healthz` + `/static/` bypass allowlist on `request.url.path`,
  which pre-`starlette` 1.0.1 is rebuilt from an unvalidated `Host` header —
  a request with `Host: x/healthz?t=` could shift `request.url.path` to a
  bypass prefix and reach protected admin endpoints unauthenticated. Bumped
  the `starlette` floor to `>=1.0.1`, **and** the middleware now reads the
  ASGI `scope["path"]` (immune to `Host` spoofing) as defence-in-depth.
- **CVE-2026-53539 (issue #113):** `python-multipart` `<0.0.30` has a
  quadratic-CPU DoS in its urlencoded-form parser. Bumped the floor to
  `>=0.0.30`.
- **Stored XSS in the geo viewer (issue #115):** the geo page injected
  sampled document data into an inline `<script>` via `json_util.dumps` — a
  document whose string `_id` contained `</script><script>…` closed the
  block and injected arbitrary JS (with access to pywebview's `js_api`).
  Feature JSON is now escaped for the script context (`<`/`>`/`&`/U+2028/
  U+2029 → `\uXXXX`) so a payload can never break out of the block.

#### Security
- `admin/middleware.py`: token-bypass check reads `scope["path"]`, not the
  Host-derived `request.url.path`.
- `admin/routers/extras.py`: `_json_for_script` escapes geo feature JSON for
  safe inline-`<script>` embedding.
- `pyproject.toml` `[admin]`: `starlette>=1.0.1`, `python-multipart>=0.0.30`.
- Regressions: `tests/test_admin_security.py`.

### Capped-collection eviction survives a backup taken mid-stream

A capped collection restored from a `backupArchive` could evict the wrong
document on its next insert — dropping the freshly-inserted row instead of the
oldest one. The cause was a stale recovery hint: the oplog-meta row that records
the next insertion sequence is only refreshed on `hello`, `prune_oplog`, and
`close` (the per-write path stopped re-persisting it because it WT-rollbacks
under concurrent writers), so a checkpoint taken between two refreshes captures a
sequence counter that lags the actual data. On reopen the server *trusted* that
stale value, re-minting an already-used natural-order sequence; the collision
overwrote a live document's entry in the insertion-order index and corrupted
capped FIFO eviction. The symptom was load- and timing-dependent — it surfaced
only when the driver's background topology `hello` happened to fall before the
last insert rather than after it.

Recovery now treats the persisted `next_seq` / `next_nat_seq` as a *hint that can
only be corrected upward*: it clamps each counter to what the oplog and
natural-order tables actually contain, so a lagging meta row can never lower the
sequence and re-mint a used value. The oplog maximum is read with a single
`prev()` (the table is keyed on the bare sequence), keeping reopen cheap.

#### Fixed

- Capped-collection FIFO eviction after restoring a `backupArchive` that was
  taken between oplog-meta refreshes — the recovered insertion-sequence counter
  is now clamped up to the natural-order table's maximum rather than trusting a
  stale persisted value, eliminating the sequence collision that dropped a
  just-inserted document. The same clamp guards the oplog `next_seq` against
  re-minting a used sequence (which would silently overwrite an oplog row).

### Storage close no longer swallows durability errors; admin history/backup never persist credentials

Two security findings from the nightly review are closed. `Storage.close()`
used to wrap its final teardown — the last oplog-meta persist, the shutdown
checkpoint, and every WiredTiger session/connection close — in bare
`contextlib.suppress(Exception)`, discarding a checkpoint or connection-close
failure with no trace at all. In a database that is a durability signal, not
noise: the embedder had no way to know the last on-disk image might be
incomplete. The teardown now logs every caught failure via
`log.exception(...)` (matching the TTL-sweep and noop-heartbeat loops) while
still completing idempotently.

The admin console's query-history store and backup helpers no longer let a
credentialed connection string reach disk or the UI in plaintext. `HistoryStore`
scrubs the URI to its password-free `display_uri()` form at the store boundary
before it becomes a SQLite lookup key, so `~/.secantus/admin.db` can never hold
a `mongodb://user:pass@host` string (and a caller passing the raw URI can't
reintroduce the leak). The mongodump/mongorestore helpers still hand the live
credential to the subprocess — they need it to authenticate — but now redact the
password from any captured stdout/stderr, closing the path where a tool that
echoes the connection string on error surfaces the secret in the rendered
backup result.

#### Fixed

- `Storage.close()` teardown errors (oplog-meta persist, shutdown checkpoint,
  WT session/connection close) are logged instead of silently suppressed
  (security issue #138).
- Admin `HistoryStore` persists the scrubbed `display_uri()` form instead of the
  raw credentialed `mongo_uri`; mongodump/mongorestore captured output has the
  password redacted (security issue #140).

### Admin console detects the target server and gates features it can't do

The admin UI is a plain pymongo client, so it can point at any of the
three MongoDB-wire servers: the SecantusDB Python server, the SecantusDB
Rust server, or a real `mongod`. They differ in which commands they
implement — most visibly the four proprietary `secantusAdmin.*`
backup/maintenance commands (no `mongod` has them) and a handful of
standard admin commands the Rust server hasn't ported yet. Until now the
console advertised every button regardless of target, so clicking
"native checkpoint backup" against a `mongod`, or "prune oplog" against
the Rust server, returned a bare `CommandNotFound`.

The console now probes the target once at connect (and on every target
swap) via `buildInfo` + `serverStatus`, classifies it as Python / Rust /
MongoDB from the server's own self-identification
(`serverStatus.secantus.server` and `buildInfo.secantusVersion`), and
derives a capability set the templates consult. A detected-server pill
appears next to the target badge, and features the target can't honour
are disabled with an explanatory tooltip instead of failing on click:
native backup archive / restore and manual oplog/TTL prune (all
`secantusAdmin.*`, SecantusDB-only), plus role grant/revoke and
connection-kill (`killOp`) where the Rust server hasn't ported them. An
unreachable or not-yet-probed target stays fully permissive, so a
transiently-down server never hides a working button.

#### Added

- `secantus.admin.capabilities`: server-capability probe + classifier
  (`classify` / `probe` / `ServerCapabilities` / `UNKNOWN`), wired into
  the app lifespan startup and `swap_target`, exposed to templates as
  `request.app.state.capabilities`.
- Admin templates gate `secantusAdmin.*` backup/maintenance buttons,
  role grant/revoke, and connection-kill to the detected server's
  capabilities, and show a server-type badge.

### Atlas Search index commands are rejected with an "Atlas" error

Atlas Search index management — the `createSearchIndexes`, `updateSearchIndex`,
and `dropSearchIndex` commands plus the `$listSearchIndexes` aggregation stage
(and its `$search` / `$searchMeta` / `$vectorSearch` siblings) — is an
Atlas-only feature. A real non-Atlas `mongod` registers these but fails them at
execution with a message naming Atlas. SecantusDB now does the same
(`CommandNotSupported`, message mentioning Atlas) instead of returning
`CommandNotFound` / an "unrecognized pipeline stage" error. Closes the
mongo-c-driver `/index-management/{list,drop,update,create}SearchIndex` tests,
which assert the error contains "Atlas".

#### Added
- `createSearchIndexes` / `updateSearchIndex` / `dropSearchIndex` command
  handlers and `$listSearchIndexes` / `$search` / `$searchMeta` /
  `$vectorSearch` aggregation-stage rejection, all returning the shared
  `aggregate.SEARCH_INDEX_ATLAS_MSG` (mirrors mongod's not-on-Atlas error).

### Dropping a collection under a tailable cursor reports "collection dropped"

When a capped collection is dropped while a tailable cursor is open on
it, the next `getMore` now fails with `QueryPlanKilled` (175) and a
"collection dropped" message — exactly what mongod surfaces to a tailing
client. Previously the cursor was simply removed, so the follow-up
`getMore` returned a bare `CursorNotFound` (43). Regular (non-tailable)
cursors are unchanged: dropping their collection still yields
`CursorNotFound`, which the strict wire gauges rely on. Closes
mongo-php-driver's `cursor-tailable_error-001`, which asserts the
dropped-collection error mentions "collection dropped".

#### Fixed
- `CursorRegistry.kill_namespace` tombstones tailable cursors (sets a
  `dropped` flag, keeps the entry) instead of deleting them; non-tailable
  cursors are still deleted. `_get_more` returns the new
  `QueryPlanKilled` "collection dropped" reply for a tombstoned tailable
  cursor, and `_drop` tombstones before the storage drop so a parked
  `awaitData` `getMore` (woken by the drop's oplog write) observes the
  flag.

### A tailable cursor's filter now applies to docs inserted after the find

A tailable + `awaitData` cursor on a capped collection re-polls the
collection for documents inserted after the `find`. That poll was
returning *every* new row, ignoring the cursor's query filter — so a
tailable cursor watching for `{a: 1}` would surface unrelated inserts
(and even pre-existing non-matching docs) the moment it ran a `getMore`.
The producer now re-applies the find filter (with the same `let` vars and
collation) to each scanned row, exactly as the oplog-tailing variant
already did. The watermark still advances past every scanned row, matched
or not, so non-matching docs aren't re-examined on the next poll. Closes
the mongo-c-driver gauge's `/Collection/tailable/timeout/single`.

#### Fixed
- `commands._find_tailable`: the capped-collection tailable producer
  filters follow-up inserts through `query.matches` instead of returning
  them unconditionally.

### A malformed `$and` / `$or` / `$nor` is a clean parse error, not a crash

`$and` / `$or` / `$nor` require a non-empty array of sub-documents. A query
passing a non-array (`{$or: true}`), an empty array, or a non-document element
used to crash the query engine — `for c in condition` raised a Python
`TypeError` that escaped the parse-error handling and surfaced over the wire as
a generic `InternalError` (1) with the traceback logged server-side. It now
matches mongod: a `BadValue` (2) parse error ("$or must be an array" / "must be
a nonempty array" / "entries need to be full objects"). Surfaced while triaging
the mongo-c-driver gauge's malformed-input command-monitoring tests.

#### Fixed
- `query._match_clause`: `$and` / `$or` / `$nor` validate their argument is a
  non-empty list of documents and raise `QueryError` (→ BadValue 2) for any
  malformed shape, instead of letting a `TypeError` leak out as InternalError.

### An unrecognised index-key string is rejected as an unknown plugin

A string value in an index key names a special index type ("plugin") — `2d`,
`2dsphere`, `text`, `hashed`. SecantusDB already accepted the geo plugins and
rejected `text`/`hashed` as out-of-scope, but it let *any other* string through:
`createIndex({abc: "hallo thar"})` silently created a broken index instead of
erroring. It now matches mongod and rejects an unrecognised plugin name with
`CannotCreateIndex` (67) "Unknown index plugin '<value>'".

This closes the mongo-c-driver gauge's `/Collection/index_w_write_concern` test,
which (after the 0.5.4b13 write-concern fix) was failing on its invalid-index
assertion — it creates `{abc: "hallo thar"}` and expects the server to reject
it. The test name is misleading; the failure had nothing to do with write
concern.

#### Fixed
- `storage.create_index`: a string index-key value that isn't a recognised
  plugin (`2d` / `2dsphere` / `2dsphere_bucket` / `geoHaystack`) is rejected
  with `CannotCreateIndex` (67) "Unknown index plugin '<value>'", alongside the
  existing `text` / `hashed` out-of-scope rejection.

### A numeric write-concern `w` above 50 is now a parse error

A `writeConcern` with a numeric `w` greater than 50 (or negative) is now rejected
at parse time with `FailedToParse` (9) and the message "w has to be a non-negative
number and not greater than 50" — matching mongod, which caps `w` at the maximum
number of voting replica-set members (50). Previously SecantusDB treated any
`w` above 1 the same way (a `CannotSatisfyWriteConcern` writeConcernError, code
100, attached to a *successful* reply). That's only correct for `1 < w <= 50` —
satisfiable on a multi-node deployment but not on our single node. Above 50 the
value is simply invalid, and mongod errors the whole command.

This closes the mongo-c-driver gauge's last cluster of failures —
`/Collection/{drop,rename,index}` and `/Database/drop`, which each run a DDL op
with `w: 99` and assert the `assert_wc_oob_error` shape (code 9, the message
above) for a server advertising version >= 4.3.3 (we advertise 7.0). The
"state-ordering" label these carried in the backlog was a misdiagnosis: the
failure is deterministic, not dependent on test order.

#### Fixed
- `commands._validate_write_concern`: a numeric `writeConcern.w` outside `[0, 50]`
  is rejected with `FailedToParse` (9) before the command runs, instead of
  falling through to the satisfiability check (code 100). `_drop_database` and
  `_rename_collection` now run this validation too (they previously skipped it,
  relying only on the dispatch-level `_unsatisfiable_wc_error`).

The `$currentOp` aggregation stage now surfaces the connecting driver's
handshake metadata — the full `clientMetadata` document (driver name/version,
OS, application name) plus a top-level `appName` — on its self-row, matching
real `mongod`. Previously only the `currentOp` *command* echoed this back; the
aggregation form (`db.aggregate([{$currentOp: {}}])`) returned a bare stub, so a
client couldn't find its own operation by `appName` or read back the metadata it
sent on connect.

This was the last real divergence the mongo-cxx-driver gauge's "integration
tests for client metadata handshake feature" exercised — it connects with
`?appName=xyz`, scans `$currentOp` for the matching op, and verifies its
`clientMetadata.{application,driver,os}`. With this and the 0.5.4b11
resume-token fix, the cxx gauge's remaining real failures are closed.

#### Fixed
- `aggregate._stage_current_op`: the `$currentOp` self-row now carries
  `clientMetadata` (the connection's `hello.client` subdoc) and a top-level
  `appName` lifted from `application.name`, threaded in via a new
  `PipelineContext.client_metadata` field that the `aggregate` command handler
  populates from the connection registry. The `currentOp` command already did
  this; the aggregation stage now matches.

### Change-stream resume tokens now advance per event, even at batchSize 1

A change stream's `postBatchResumeToken` now tracks the resume token of the
**last event actually returned in each batch**, not the last event the server
happened to prefetch. The producer reads up to 200 oplog rows ahead while the
cursor hands them back `batchSize` at a time, so with a small batch size the
token reported on each `getMore` was stale — three single-event reads all
carried the same token. Drivers that resume off the per-batch token (the whole
point of `postBatchResumeToken`) would resume from the wrong place. Alongside
it, an empty `getMore` over a quiet collection no longer re-mints the token with
a fresh cluster time when the oplog tail hasn't actually moved, so an exhausted
stream reports the same resume token as its last event rather than drifting.

This was surfaced by the mongo-cxx-driver gauge's spec prose test "ChangeStream
must continuously track the last seen resumeToken" (`batchSize=1`, read three
events, assert each token differs, then assert the post-exhaustion token equals
the last event's). The fix is server-side and driver-agnostic — every change-
stream driver benefits.

#### Fixed
- `commands._change_stream_cursor_doc`: `postBatchResumeToken` is now the `_id`
  (resume token) of the last event in the returned batch, not the producer's
  prefetch-tail `last_token` — so per-batch tokens advance correctly under any
  `batchSize`.
- `commands` change-stream producer: an empty `getMore` only advances /
  re-mints the resume token when the oplog tail has genuinely moved past the
  cursor's position, preserving the go `resume_token_updated_on_empty_batch`
  advance while fixing the mongocxx no-change-equals-last-token case. (On a
  truly quiet collection the token now advances via the oplog's periodic noop
  heartbeats, mirroring mongod's `periodicNoopIntervalSecs` — not a per-getMore
  clock tick.)
- The change-stream batch builder (`_change_stream_cursor_doc`) is shared with
  capped-collection tailable cursors, whose documents carry plain `_id` values
  and no resume token. The new `postBatchResumeToken` = last-event-`_id` logic
  is gated to change-stream cursors only, so a capped tailable getMore no longer
  emits a non-document PBRT (strict drivers — the Java driver — rejected an
  int32 there).

### Two more conformance gauges: the Kotlin driver and pymongo async

SecantusDB now also measures itself against the official MongoDB **Kotlin**
driver and against pymongo's native **async** (`AsyncMongoClient`) suite —
bringing the gauge count to thirteen. Both reuse infrastructure already in the
tree rather than vendoring new submodules: the Kotlin driver ships *inside* the
mongo-java-driver monorepo (`driver-kotlin-sync`), so its gauge runs the
`:driver-kotlin-sync:integrationTest` Gradle task against an embedded SecantusDB
daemon over the same JDK/Gradle toolchain the Java gauge already needs; and the
async gauge points pymongo's `test/asynchronous/` suite at the same
embedded-server plugin the sync gauge uses, run under `pytest-asyncio` with
`asyncio_mode=auto`. The async gauge is the more interesting of the two — it
exercises the async/await wire path that replaced Motor, which drives cursors and
change-stream `getMore` polling through a different event-loop code path than the
synchronous client, catching divergences a sync-only gauge can't see.

Run them with `invoke validate-pymongo-async` and `invoke validate-kotlin`. Both
join the weekly `validate.yml` matrix and `invoke validate-all`. Neither touches
the shipped `secantus` package — the gauge directories are dev-only, excluded
from the wheel and sdist like every other gauge.

Bringing the Kotlin gauge up also surfaced and fixed a latent break in the
shared gauge plumbing: every daemon-subprocess gauge (go / node / java / ruby /
rust / c / cxx / dotnet) passes `--log-level WARNING`, but `gauge_common.
spawn_daemon` learns the daemon's kernel-assigned port by grepping its
`listening on <host>:<port>` line — which the Python server logs at INFO, so
WARNING suppressed it and the spawn waited the full timeout (and, with a
blocking read, could hang for hours — one scheduled CI run was cancelled at 6h).
The spawn now forces the Python daemon to INFO (per-request logging is at DEBUG,
so this adds only the one readiness line, no noise) and reads the daemon's output
under a hard deadline so a missing line times out instead of hanging. This is
why those gauges were red in the weekly run; they go green again with this fix.

#### Added
- `pymongo_async_validation/` gauge package (`include_paths` / `generate_report`)
  and the `invoke validate-pymongo-async` task — pymongo's native
  `AsyncMongoClient` suite against an embedded SecantusDB, reusing
  `pymongo_validation.plugin` and the `vendor/pymongo-tests` submodule. Adds a
  `pytest-asyncio` dev dependency.
- `kotlin_validation/` gauge package (`include_modules` / `runner` /
  `generate_report` / `init.gradle.kts`) and the `invoke validate-kotlin` task —
  the official Kotlin driver's `:driver-kotlin-sync:integrationTest` suite against
  a standalone SecantusDB daemon, sharing the Java gauge's JVM toolchain and
  `vendor/mongo-java-driver` submodule.
- Both gauges wired into `invoke validate-all` / `validate-all-servers` and the
  CI `validate.yml` matrix.

#### Fixed
- `gauge_common.spawn_daemon`: the Python daemon is now forced to `--log-level
  INFO` so its `listening on …` readiness line (logged at INFO) is visible even
  though gauges pass `--log-level WARNING`, and the readiness read is bounded by
  the spawn deadline instead of a blocking `readline()`. Unbreaks the
  daemon-subprocess gauges (go / node / java / ruby / rust / c / cxx / dotnet),
  which were failing/hanging in CI because WARNING suppressed the line.

### Closing the gaps the C, C++, and C# gauges opened

Adding the mongo-c-driver, mongo-cxx-driver, and mongo-csharp-driver gauges
turned up a cluster of small conformance divergences in the Python server, and
this release fixes the actionable ones. None were show-stoppers on their own —
they're the kind of edge that a permissive driver like pymongo glosses over but
a strict C extension or a spec-faithful CRUD suite pins exactly — and together
they tighten how faithfully SecantusDB answers the corners of the wire protocol.

The headline is document-validation error detail: a write that fails a
collection validator now returns mongod's full per-operator `errInfo.details`
(`operatorName` / `specifiedAs` / `reason` / `consideredValue` / `consideredType`)
instead of a bare placeholder, so a driver can tell you *why* a document was
rejected. Alongside it: `$out` / `$merge` are now rejected unless they're the
final pipeline stage, they enforce the destination collection's validator
(honouring `bypassDocumentValidation`), a change stream opened with an invalid
`$match` errors immediately rather than on the first batch, `batchSize` accepts
any BSON number type, over-long database names are rejected, dropping or renaming
a collection invalidates its open cursors, and `collMod` can stage an index
`prepareUnique` and convert it to `unique` — reporting the duplicate `_id`
groups as `violations` when the conversion can't proceed.

#### Added
- `collMod {index: {prepareUnique: true}}` arms an existing index so new
  uniqueness-violating writes are rejected (11000) while pre-existing duplicates
  remain, and `collMod {index: {unique: true}}` converts it — refusing with
  `CannotConvertIndexToUnique` (359) plus a `violations: [{ids: [...]}]` array
  when duplicates exist (`storage.set_index_options` / `find_index_duplicates`).

#### Fixed
- Document validation now synthesises mongod's per-operator `errInfo.details`
  for query-expression validators (`commands._validation_failure_details`),
  used by both the insert path and `findAndModify` upsert simulation.
- `$out` / `$merge` are rejected with `Location40601` (40601) unless they are the
  final pipeline stage, and they enforce the destination collection's `validator`
  unless `bypassDocumentValidation` is set (`DocumentValidationFailure`, 121).
- Change streams validate `$match` filter syntax at open time, so an unknown
  query operator errors at `.begin()` (aggregate) rather than the first `getMore`.
- `find` / `aggregate` accept a `batchSize` encoded as any BSON number, including
  `Decimal128`.
- Commands targeting a database whose name exceeds 63 bytes are rejected with
  `InvalidNamespace` (73).
- Dropping or renaming a collection now kills its open cursors, so a later
  `getMore` fails with `CursorNotFound` (43) instead of serving stale rows.

### createIndexes now rejects conflicting index definitions

`createIndexes` previously accepted a re-creation that collided with an existing
index, silently treating it as a no-op. It now matches `mongod`: re-using an
index name for a **different key spec** is rejected with `IndexKeySpecsConflict`
(code 86), re-using a name with the **same key but different options** is
rejected with `IndexOptionsConflict` (code 85), and an **identical** re-create
returns `note: "all indexes already exist"` so drivers report it as a no-op
rather than a fresh build. This was surfaced by the mongo-cxx-driver gauge
(`create_index tests/fails`, `index_view/fails for same name`, `fails for same
keys and options`), and applies to every driver's `createIndex` / index-view
API.

#### Fixed
- `createIndexes`: same-name-different-key now errors `IndexKeySpecsConflict`
  (86); identical re-creates now carry `note: "all indexes already exist"`
  (`storage.create_index` / `commands._create_indexes`). Same-name-different-
  options continues to error `IndexOptionsConflict` (85).

### An eleventh conformance gauge: the MongoDB C# / .NET driver

SecantusDB now also measures itself against the official MongoDB **C# / .NET**
driver — the one the .NET, Unity, and Xamarin ecosystems build on — bringing the
gauge count to eleven. The gauge runs the driver's own xUnit suite via `dotnet
test` against an embedded SecantusDB daemon, with `MONGODB_URI` pointed at it,
scoped to the CRUD specification conformance tests
(`MongoDB.Driver.Tests.Specifications.crud`). `MongoDB.Driver.Tests` as a whole
is enormous and dominated by non-server unit tests and external-service suites
(client-side encryption, Atlas Search, multi-node transactions), so the CRUD
spec runner is the focused, bounded conformance slice — expandable to more spec
families over time. The driver's `[RequireServer]` attribute self-skips tests
whose server-version or topology requirements a single node doesn't meet.

Run it with `invoke validate-dotnet` (needs the .NET SDK and `gpg` — the driver's
encryption project verifies a downloaded libmongocrypt during build). It joins
the weekly `validate.yml` matrix and the cross-driver summary.

#### Added
- `dotnet_validation/` gauge package (`runner` / `generate_report` /
  `include_paths`) and the `invoke validate-dotnet` task; wired into `invoke
  validate-all`, the cross-driver summary (`validation_summary`), and CI.

### Two new conformance gauges: the MongoDB C and C++ drivers

SecantusDB now measures itself against the official MongoDB **C** (`libmongoc`)
and **C++** (`mongocxx`) drivers — bringing the gauge count to ten. libmongoc is
the lowest-level official client, the one the PHP, Ruby, and PyMongo
C-extensions ultimately wrap; mongocxx is the modern C++ driver built on top of
it. Both gauges build the driver's own test suite from source and run it,
unmodified, against an embedded SecantusDB daemon — `test-libmongoc` (curated
CRUD / cursor / aggregation / command / GridFS / index suites over
`MONGOC_TEST_URI`) for C, and the mongocxx `test_driver` Catch2 suite for C++.
Strict native clients surface type- and wire-shape divergences that more
permissive drivers accept silently.

Two wrinkles were worth solving. First, libmongoc's test fixture probes the
server with `replSetGetStatus` and aborts the whole run on an unexpected error,
so SecantusDB now answers it like a standalone `mongod` —
`NoReplicationEnabled` (code 76), "not running with --replSet" — which driver
harnesses special-case as "standalone, skip the replica-set-only paths". Second,
mongocxx's tests hard-wire `mongodb://localhost:27017` with no environment
override, so the C++ gauge binds its daemon on port 27017 and refuses to run if
something else already holds it (it won't gauge a foreign server).

Run them with `invoke validate-c` / `invoke validate-cxx` (both need `cmake` and
a C/C++ toolchain; the first run builds the drivers, later runs reuse the cached
builds). Both join the weekly `validate.yml` matrix and the cross-driver summary.

#### Added
- `c_validation/` and `cxx_validation/` gauge packages (`runner` /
  `generate_report` / `include_paths` each) and the `invoke validate-c` /
  `invoke validate-cxx` tasks; wired into `invoke validate-all`, the cross-driver
  summary (`validation_summary`), and CI.
- `replSetGetStatus` command: returns the standalone-mongod
  `NoReplicationEnabled` error so single-node-aware driver test fixtures skip
  replica-set-only behaviour instead of aborting.

### The in-process Rust engine selection is fully removed

The transitional in-process engine selection — `SECANTUS_ENGINE=python|rust|auto`,
the `--engine` CLI flag, and the `SecantusDBServer(engine=...)` constructor
parameter — has been removed entirely. It was already inert since 0.5.3b3 (no
operator module delegated to the `_secantus_core` extension any more), so this is
a no-op for behaviour: the Python server has been pure-Python end to end for
several releases. This change deletes the dead surface so there is one obvious
way things work.

SecantusDB's Rust implementation lives in the **separate Rust server** (and the
standalone `secantusdb` binary), not in this package's request path. The
`secantus-core` wheel remains as the engine library and the parity-test oracle
that pins each Rust engine byte-for-byte against its pure-Python counterpart.

#### Removed
- The `--engine` CLI flag and the `engine=` parameter on `SecantusDBServer`.
  Passing `engine=` now raises `TypeError` (the flag had no effect since 0.5.3b3).
- The `secantus.engine` module (the inert compatibility stub) and its
  `tests/test_engine.py`.
- The `SECANTUS_ENGINE=rust` full-suite CI step, which re-ran the test suite
  under a now-dead env var and was a source of flaky worker-crash failures.

### Point-in-time recovery: restore the database as it was at any moment in its oplog

SecantusDB already kept a mongod-shaped oplog and could take consistent
WiredTiger backup archives; this release joins the two into real point-in-time
recovery. Given a backup (or a stopped server's data directory), you can now
rebuild a fresh database as it was at any target timestamp by replaying the
oplog forward — documents, in-place updates, deletes, collection options
(`capped` / `size` / `max` / `validator` / `viewOn` / …), and index / `collMod` /
rename DDL are all reconstructed through the ordinary write paths, so the result
is indistinguishable from the live database at that instant.

Recovery is offline, matching real `mongod`: it writes a fresh data directory
you then start a new server on. Drive it from the CLI (`secantusdb restore
--source <archive|dir> --target-dir <dir> [--to-time <ISO> | --to-timestamp
<secs,ord>]`) or the `secantusAdmin.restoreToTimestamp` admin command. A
multi-document transaction is always replayed all-or-nothing — its statements
share one commit timestamp, so a recovery point never lands mid-transaction.
Out of the box the recovery window is the oplog retention window (tune
`--oplog-retention-seconds` / `--oplog-max-entries` for the horizon you need). To
reach further back, turn on oplog archiving (`--oplog-archive-dir`) and take
periodic base snapshots: SecantusDB then keeps the dropped oplog on disk and
recovery stitches the newest snapshot before your target together with the
archived oplog, so any moment in the archived history is reachable without keeping
the whole oplog live. See [Backup & point-in-time recovery](recovery.md).

#### Added
- `secantus.diff.apply_update_description` — applies a `$v: 2`
  `updateDescription` back to a document (the inverse of
  `compute_update_description`); the keystone of oplog replay.
- `secantus.oplog_replay` — `replay()` / `restore_to_timestamp()` /
  `restore_archive_to_timestamp()`: replays an oplog source into a fresh store,
  stopping at a target `ts` / wall-clock time.
- `Storage.replay_mode()` — a context manager that suppresses oplog emission so
  replay drives the real write paths without regenerating the oplog.
- `secantusdb restore` CLI subcommand and the `secantusAdmin.restoreToTimestamp`
  wire command.
- Backup archives now embed a `pitr-manifest.json` describing their recoverable
  oplog range (`Storage._pitr_manifest`).
- Collection options (`capped` / `size` / `max` / `validator` / `viewOn` / …) now
  ride the `create` oplog entry, so PITR replay reconstructs them on the restored
  collection (previously only documents and indexes were). `Storage.create_collection`
  gained an `options=` argument; the same options now also surface in the
  `show_expanded_events` change-stream `create` event's `operationDescription`.
- `--preserve-oplog` (`secantusdb restore`) / `preserveOplog: true`
  (`secantusAdmin.restoreToTimestamp`) carries the replayed oplog onto the
  restored directory verbatim, so a change stream on the restored server can
  resume from a token minted *before* the restore point. The default still starts
  a fresh oplog timeline, matching `mongorestore`. Backed by
  `Storage.import_oplog_segment`.
- **PITR v2 — arbitrary-window recovery** (`secantus.pitr_archive`). A server
  started with `--oplog-archive-dir <dir>` (`[oplog] archive_dir`) archives the
  oplog rows `prune_oplog` is about to drop into durable segment files first;
  `secantusAdmin.archiveBaseSnapshot` / `Storage.archive_base_snapshot` take base
  snapshots into the same directory. Recovery then accepts that **archive
  directory** as the `restore` source (CLI / wire auto-detect it): it picks the
  newest base snapshot at or before the target time and stitches the archived
  oplog forward onto it. This lifts the v1 genesis-intact restriction — a restore
  can reach a time *before* the live oplog floor, without keeping the entire oplog
  live. Base snapshots are taken on demand (no background scheduler, matching
  `prune_ttl` / `prune_oplog`).

## [0.5.3b13] — 2026-06-16

### `find()` with no sort now returns documents in insertion order

An unsorted `find()` now returns documents in **insertion order**, matching
`mongod`'s natural (storage) order. Previously SecantusDB returned them in `_id`
order — which coincides with insertion order for the default monotonic
`ObjectId` `_id`s, but diverged whenever a collection mixed `_id` types or used
non-monotonic `_id`s (an int `1`, a string `"foo"`, a sub-document — BSON sorts
those very differently from the order you inserted them). Code and drivers that
read back rows in the order they were written — a common, reasonable assumption
that `mongod` honours — now see the same order here.

Internally this adds a small natural-order index (a monotonic insertion sequence
→ document map) that an unsorted scan and the `$natural` hint walk; the document
store itself is unchanged, so every `_id` lookup, secondary index, and
uniqueness check is untouched. Capped-collection eviction and equal-key sort
tie-breaks also follow insertion order now. (Multi=false `updateOne`/`deleteOne`
without a sort still pick the `_id`-order-first match rather than the
insertion-first one — a smaller remaining divergence, tracked in the backlog.)

#### Fixed
- Unsorted `find()` (and the `$natural` hint) return documents in insertion
  order, matching `mongod` — including for collections with mixed-type or
  non-monotonic `_id`s. Clears the PHP `BulkWrite::testInserts` /
  `bulkwrite-insert-004` conformance tests.

## [0.5.3b12] — 2026-06-16

### `count` honours a `hint`, and 2dsphere indexes report their version

Two driver-conformance fixes. `count` now respects a `hint`: pass an index name
or key pattern and the count walks that index — so hinting a **sparse** index
counts only the documents present in it. `count({}, hint: "sparse_idx")` returns
the number of docs that have the indexed field, not the whole collection, exactly
as `mongod` does. Previously `count` ignored the hint and always counted every
document.

Separately, `2dsphere` indexes now carry a `2dsphereIndexVersion` in their
`listIndexes` output (version 3, mongod's format since 3.2), so drivers that
introspect geo indexes — like the PHP library's `IndexInfo::is2dSphere()` — read
the field they expect.

#### Added
- `2dsphereIndexVersion` (3) on `2dsphere` indexes, surfaced via `listIndexes`.

#### Fixed
- `count` now honours `hint` (index name or key pattern), counting via the
  hinted index — including sparse-index semantics (missing-field docs excluded).

## [0.5.3b11] — 2026-06-16

### `serverStatus` reports a live open-cursor count

`serverStatus` now includes `metrics.cursor.open.total` — the number of cursors
currently registered on the server. It rises by one when a batched query leaves
a cursor open for `getMore` and returns to its baseline once the cursor is
exhausted or killed (via `killCursors`, which drivers send when a cursor object
is destroyed). Tools and drivers that watch cursor lifecycle — including the PHP
extension's cursor-destruct test — can now see cursors open and close.

The value is read live from the server's `CursorRegistry`, so it reflects the
true set of not-yet-exhausted, not-killed cursors at the moment of the call.

#### Added
- `serverStatus.metrics.cursor.open.total` (live open-cursor count from the
  `CursorRegistry`).

## [0.5.3b10] — 2026-06-16

### `collMod` can retune a TTL index's expiry

`collMod` now handles its `index` modification form: pass
`{collMod: "<coll>", index: {keyPattern: {...}, expireAfterSeconds: N}}` (or
`{index: {name: "<idx>", expireAfterSeconds: N}}`) and SecantusDB rewrites the
TTL index's expiry in place, returning the `expireAfterSeconds_old` and
`expireAfterSeconds_new` pair that `mongod` echoes. The new value takes effect
immediately — `prune_ttl` reads the expiry from the same index options — so a
retuned TTL window applies on the next prune.

Previously `collMod` accepted the command but ignored the `index` form,
returning a bare `{ok: 1}` with neither the old nor new expiry. The PHP
library's `ModifyCollection` and `Database::modifyCollection` tests assert both
values; both now pass.

#### Added
- `collMod` `index` form for TTL retuning: resolves the target index by
  `keyPattern` or `name`, updates `expireAfterSeconds`, and returns
  `expireAfterSeconds_old` / `expireAfterSeconds_new`. Backed by a new
  `Storage.set_index_expiry`.

## [0.5.3b9] — 2026-06-16

### Duplicate-key errors now read exactly like `mongod`

When a write hits a unique-index collision, the `E11000` error message now
matches `mongod` verbatim: `E11000 duplicate key error collection: <db>.<coll>
index: <indexName> dup key: { <field>: <value> }`. Previously SecantusDB emitted
a terser, non-standard form (`… in index <name>: _id=<value>`) that worked for
permissive drivers but failed the type-strict ones that pin the message text —
the PHP extension's `WriteError::getMessage()` and `WriteResult::getWriteErrors()`
assert the full string, down to the shell-formatted `dup key` fragment.

The structured fields drivers parse — `code`, `index`, `keyPattern`, `keyValue`
— were already correct; this is purely the human-readable message catching up,
across every path that can raise a duplicate key (batch insert, update/upsert,
and unique-index builds). The fix clears the PHP extension's `writeError` and
`writeResult` suites.

#### Fixed
- Duplicate-key (`E11000`) error messages now use `mongod`'s exact wording,
  including the `collection: <ns> index: <name> dup key: { … }` shape with
  shell-formatted key values, consistently across all duplicate-key raise sites.

## [0.5.3b8] — 2026-06-16

### `explain` now speaks `allPlansExecution`, and aggregate's inline explain flag works

`explain` gained the last two pieces the official MongoDB drivers probe for. At
the most verbose level, `verbosity: "allPlansExecution"`, the reply now carries
an `allPlansExecution` array inside `executionStats` — empty, because a
single-node query is always served by a single candidate plan with no rejected
alternatives, which is exactly what real `mongod` reports when there's no
multi-planning to summarise. Drivers that assert the key is present (and absent
at lower verbosities) now see the shape they expect.

The `aggregate` command also learned to honour its legacy inline `explain: true`
flag — the form drivers send when you call `explain()` on an aggregation rather
than wrapping it in the top-level `explain` command. SecantusDB previously ran
the pipeline and returned data; it now returns the explain plan instead, and
critically does **not** execute a trailing `$out` or `$merge` write stage under
explain, matching `mongod`'s dry-run behaviour. Together these clear the PHP
library's entire `ExplainFunctionalTest` and aggregate-explain suite.

#### Fixed
- `explain` with `verbosity: "allPlansExecution"` now includes an
  `executionStats.allPlansExecution` array (empty for single-solution plans),
  on both `find`-style and `aggregate` explains.
- The `aggregate` command's inline `explain: true` flag now returns the explain
  document (`stages` / `queryPlanner`) instead of running the pipeline, and
  suppresses `$out` / `$merge` writes under explain.

## [0.5.3b7] — 2026-06-15

### `$exists: true` rides a sparse index instead of scanning the collection

A query of the form `{field: {$exists: true}}` now uses a sparse single-field
index on `field` when one exists, instead of falling back to a full collection
scan. A sparse index holds an entry for exactly the documents where the field is
present — missing-field documents are omitted, present-but-`null` and array
values keep an entry — so the complete set of index entries *is* the
`$exists: true` match set. The planner walks the whole index (no value bound),
and `explain` reports `IXSCAN` accordingly. A non-sparse index still can't serve
`$exists: true` (it has an entry per document, including the absent ones), and
`$exists: false` never uses a sparse index — both correctly stay on `COLLSCAN`.
Results were always correct; this is the missing fast path.

#### Added

- `{field: {$exists: true}}` uses a sparse single-field index (IXSCAN) when one
  is present, via `Storage._sparse_index_for_exists` + `_all_id_keys_for_index`,
  mirrored in `explain_plan`. Non-sparse indexes and `$exists: false` stay on
  COLLSCAN.

#### Fixed

- The three pymongo DBRef-spec tests (`test_dbref.py::TestDBRefSpec`) are now
  deselected from the gauge. They are pure client-side BSON codec tests that
  never exercise SecantusDB; they pass under plain unittest but crash the
  gauge's `-n1` xdist worker because execnet can't pickle the `ObjectId` in
  their `subTest` params (`DumpError`). Deselecting them keeps the gauge run
  clean and stops three spurious failures from being attributed to the server.

### Fixed a shutdown race that could crash the server process

Stopping a `SecantusDBServer` now drains its in-flight per-connection threads
before tearing down WiredTiger. Previously `stop()` joined only the accept
thread and then closed the storage engine — so a connection handler still
mid-WiredTiger-operation (e.g. a change-stream tailable `getMore` reading the
oplog) had its WT connection freed underneath it: a use-after-free that surfaced
as an intermittent native crash (the pytest-xdist worker death seen near the end
of the full suite under churn). `stop()` now closes every connection socket to
unblock reads, wakes any tailable `getMore` parked on the oplog condition
variable, and waits for the active-connection count to reach zero before calling
`storage.close()`. A 200-iteration stress that reliably tripped the use-after-
close now runs clean.

Waking those parked reads is platform-specific, and the first cut got it wrong
on both ends. On POSIX, `shutdown(SHUT_RDWR)` wakes a `recv` blocked in another
thread while leaving the descriptor valid; calling `close()` from the stopping
thread instead does *not* wake the parked `recv` and frees the fd number for
immediate reuse, leaving the handler blocked forever on a recycled descriptor —
so the drain barrier timed out. On Windows the opposite holds: `shutdown` does
not interrupt an already-blocked `recv`, so `closesocket` is required. The wake
is now `shutdown`-only on POSIX and `shutdown`-then-`close` on Windows. The drain
barrier also re-runs the socket wake on every poll, not just once up front: the
accept thread bumps the active-connection count and spawns the handler *before*
the handler registers its socket, so a connection accepted in the instant before
`stop()` could register after the initial sweep and never be woken — re-sweeping
catches it within milliseconds.

#### Fixed

- `SecantusDBServer.stop()` drains in-flight connection threads before closing
  WiredTiger (via `ConnectionRegistry.close_all` + `Storage.signal_shutdown` +
  an active-connection drain barrier), eliminating a use-after-free / native
  crash on teardown under load.
- The stop-time socket wake is now platform-correct: `shutdown`-only on POSIX
  (closing the fd from another thread left handlers blocked on a recycled
  descriptor and timed out the drain), `shutdown`+`close` on Windows (where
  `shutdown` alone doesn't interrupt a blocked `recv`). The drain barrier
  re-sweeps each poll so a connection that registers its socket just after
  `stop()` begins is still woken.

### Tailable cursors over `local.oplog.rs`

A client can now tail the oplog the way replication does: `local.oplog.rs`
accepts `TAILABLE_AWAIT` find cursors and streams oplog entries as they're
written. Two pieces landed for this — the synthetic oplog view is now reported
as a capped collection by `collection_is_capped` (so a tailable cursor isn't
rejected), and a dedicated oplog tailable producer reads new entries by oplog
seq (oplog documents have no `_id`, so the ordinary capped-collection tail path
doesn't apply). `find().sort("$natural", ...)` is honoured against the view —
the oplog's only meaningful order.

To match mongod — whose oplog is never empty (its first entry is the replica
set's "initiating set" noop) — a freshly-started server now seeds one bootstrap
noop into the oplog, so a client can tail `local.oplog.rs` before any user
write. The seed is an `op: "n"` entry (skipped by change-stream projection, so
it never surfaces as a change event) and only fires on a truly fresh oplog.
Closes the pymongo gauge's `test_cursor.test_to_list_tailable`.

#### Added

- `TAILABLE_AWAIT` find over `local.oplog.rs` (via `_find_tailable_oplog`), and
  `$natural` sort on the oplog view.
- A bootstrap oplog noop seeded at server start (`Storage.ensure_oplog_bootstrap`)
  so `local.oplog.rs` is never empty, matching mongod.

### The Python server is pure Python — no Rust dependency — and preserves numeric types

The `secantus` package no longer imports or calls any Rust component. The
original in-process engine-swap — where each operator module could delegate to
the optional `_secantus_core` extension under `SECANTUS_ENGINE=rust` — has been
retired in favour of the two-separate-servers model: the Python server is the
pure-Python implementation, end to end, and the Rust engines live only in the
standalone Rust server (and in the parity-oracle test suites, which import the
extension directly rather than through this package). `secantus.engine` remains
as an inert compatibility stub so `SecantusDBServer(engine=...)` keeps working.

Decoupling the engines let the Python operator engines adopt MongoDB's numeric
type promotion (int32 < int64 < double < decimal128) without being pinned to a
not-yet-updated Rust port. `$inc`, `$mul`, and the `$sum` accumulator now
preserve the BSON numeric type of their result — `Int64(5)` incremented by `3`
stays `Int64(8)` instead of narrowing to int32 on the wire — so a client codec
that keys on the BSON 64-bit type round-trips correctly. This closes the pymongo
gauge's `test_custom_types` aggregate/findAndModify decoder cases.

#### Changed

- `secantus` is now pure Python with no Rust import in the request path; the
  `SECANTUS_ENGINE` in-process accelerator is retired (the Rust engines moved to
  the standalone Rust server). `secantus.engine.available()` / `enabled()`
  always report Python.

#### Fixed

- `$inc` / `$mul` / `$sum` preserve the BSON numeric type per mongod's promotion
  rules (int32 < int64 < double < decimal128) via the new `secantus.numerics`
  helpers, instead of narrowing 64-bit results to int32.

### `find` honours `returnKey` and `showRecordId`

`find` now supports the `returnKey` and `showRecordId` cursor options. With
`returnKey: true` each result is reduced to just the keys of the index that
serves the query — the index's key-pattern fields plus the sort fields (a sort
by `_id`, served by the document table's natural order, yields `{_id: <value>}`).
With `showRecordId: true` each document is tagged with a `$recordId`; when
`returnKey` is also set, `showRecordId` adds nothing, matching `mongod`. Closes
the pymongo gauge's command-monitoring `find with showRecordId and returnKey`.

#### Added

- `returnKey` (project results down to the serving index's key fields) and
  `showRecordId` (`$recordId` tag) options on the `find` command.

### `createIndexes` accepts and ignores the deprecated `dropDups` option

`dropDups` was removed in MongoDB 3.0, but modern `mongod` still accepts it on
the wire and silently ignores it rather than rejecting the index spec. SecantusDB
now matches that: passing `dropDups` no longer trips the unknown-field guard.
The practical upshot is that building a `unique` index over data that already
contains a duplicate fails on the duplicate with `DuplicateKey` (11000) — a
`DuplicateKeyError` to the driver — exactly as a real server does, instead of an
unrelated "unknown field" error. The collection is left untouched and no index is
created. Closes the pymongo gauge's `test_collection.test_index_dont_drop_dups`.

#### Changed

- `createIndexes` accepts `dropDups` and strips it from the stored index
  options (deprecated, ignored — never drops duplicates).

### Partial indexes serve range-on-indexed-field queries with a residual clause

A query that puts a range on a partial index's indexed field and an extra
clause that the index's partial filter absorbs now uses the index — e.g.
`find({x: {$gt: 1}, a: 1})` against an index on `x` with
`partialFilterExpression: {a: {$lte: 1.5}}`. The `x` range rides the index,
the `a: 1` clause is implied by the partial filter (so the index's existence
already guarantees it) and is rechecked by the exact post-scan matcher, and
`explain` reports `IXSCAN` with `isPartial: true`. Previously any multi-field
filter fell off the single-field index path to a COLLSCAN.

The relaxation is deliberately conservative: only *partial* indexes get this
treatment, and only when every residual field is a partial-filter field, so a
non-partial residual still keeps the query on a collection scan. This closes
the last open assertion in the pymongo gauge's `test_collection.test_index_filter`.

#### Changed

- The single-field index lookup and its `explain` mirror now accept a
  multi-field filter when the non-indexed fields are absorbed by an implied
  partial filter, via a shared `_single_field_partial_residual_match` selector.

### Tailable cursors die on capped-collection rollover

A tailable cursor over a capped collection now dies with `CappedPositionLost`
when the collection rolls over and evicts the document the cursor was anchored
on — exactly as `mongod` does. Before, the cursor would blithely keep
streaming the post-rollover documents instead of recognising it had been
lapped. The server detects this by comparing the cursor's last-returned
position against the collection's current oldest document; if the anchor has
been evicted it returns error 136, which `pymongo` swallows for tailable
cursors (the cursor reports `alive == False` and the in-flight read yields
nothing). Closes the pymongo gauge's `test_cursor.test_tailable`.

#### Fixed

- Tailable cursors on capped collections now surface `CappedPositionLost`
  (code 136) when rollover evicts their anchor document, instead of
  continuing to stream the rolled-over documents.

### Change streams report create, modify, and richer DDL events

Change streams opened with `showExpandedEvents: true` now surface the full
set of expanded DDL events that `mongod` 6.0+ emits. A `createCollection`
(including views) produces a `create` event, a `collMod` produces a
`modify` event, and `rename` events carry an `operationDescription` with
the destination namespace and the dropped target's UUID under
`dropTarget`. CRUD events (insert / update / delete / replace) on an
expanded stream also carry the watched collection's `collectionUUID`, the
way a real server tags them.

Previously only `createIndexes` / `dropIndexes` were emitted as expanded
events; `create` and `modify` had no oplog entry at all, so a stream
waiting for them blocked indefinitely. This completes the
`showExpandedEvents` spec surface that single-node SecantusDB can support
(sharding-only events like `shardCollection` remain out of scope), taking
the pymongo change-stream gauge from 102 to 106 passing — a clean sweep of
`test_change_stream.py`.

#### Added

- `create` (createCollection / views) and `modify` (collMod) change-stream
  events under `showExpandedEvents`, both gated off by default like the
  other expanded events.
- `operationDescription.{to,dropTarget}` on expanded `rename` events, and
  `collectionUUID` on expanded CRUD events.

### Resumed change streams return their backlog on open

Opening a change stream with `resumeAfter`, `startAfter`, or
`startAtOperationTime` now returns the already-committed backlog — the
events between the resume point and now — in the aggregate's `firstBatch`,
exactly as `mongod` does. Previously every change-stream open returned an
empty `firstBatch` and deferred all events to the first `getMore`. That
was invisible to most consumers, but a driver that inspects the cursor
for buffered data *before* issuing any `getMore` (pymongo's
`CommandCursor._has_next()`, which never sends one itself) saw nothing
and reported the stream as empty.

A fresh tail watch has no backlog, so it still opens with an empty
`firstBatch` — the change is scoped to the resuming forms. And because a
non-empty `firstBatch` means pymongo doesn't overwrite its cached resume
token from the open response, an uniterated resumed stream now correctly
reports `resume_token` equal to the token the caller passed in. Closes
the pymongo gauge's `test_resumetoken_uniterated_nonempty_batch_*`
(change-streams prose test #14), lifting the change-stream gauge from
100 to 102 passing.

#### Fixed

- Resumed change-stream opens (`resumeAfter` / `startAfter` /
  `startAtOperationTime`) return their committed backlog in `firstBatch`
  instead of deferring every event to the first `getMore`, so a driver
  that checks for buffered data before any `getMore` sees the events and
  an uniterated resumed stream reports the correct `resume_token`.

### Profiler op-class for `distinct` and `count`

`system.profile` entries for `distinct` and `count` are now recorded
under `op: "command"`, matching `mongod` — where only `find` carries
`op: "query"`. The previous bucketing filed both under `op: "query"`, so
a profile query like `{op: "command", "command.distinct": "<coll>"}`
found nothing. Monitoring tooling that slices the profiler by operation
class now sees the same shape it would against a real server.

This closes the pymongo gauge's `test_cursor.test_comment`. The OP_MSG
exhaust-cursor mid-stream-fault hardening shipped earlier this cycle
also gained a dedicated regression test (a synthetic mid-stream
`getMore` fault must terminate the stream with a `moreToCome`-clear
reply, never drop the connection).

#### Fixed

- `distinct` / `count` profiler entries use `op: "command"` (were
  `op: "query"`), so `system.profile` queries that filter by operation
  class find them.

### OP_MSG exhaust cursors

Exhaust cursors (`CursorType.EXHAUST`) now stream over the wire the way
a real `mongod` does. When a driver sets the OP_MSG `exhaustAllowed`
flag on a `getMore`, SecantusDB streams every remaining batch back over
the same socket using the `moreToCome` flag — one round trip instead of
a `getMore` per batch — and closes the stream with a trailing empty
reply carrying `id: 0`. That trailing empty batch is what makes a real
server keep the cursor alive until the client has drained it; pinning it
faithfully is why pymongo's command monitor sees `find, getMore,
getMore, getMore` for three documents at `batchSize: 1`, and why
exhaust-pinned connections return to the pool at exactly the right
moment.

This closes the last wire-protocol gap behind the pymongo gauge's
`test_exhaust` / `test_exhaust_cursor_db_set` cases. The streaming is
driven entirely in the connection loop (`SecantusDBServer._stream_exhaust_getmore`)
off the existing cursor registry, so no operator engine or storage path
changed; `find` / `aggregate` replies that open a cursor are still sent
as a single message (mongod streams only on `getMore`).

#### Added

- OP_MSG exhaust-cursor streaming: a `getMore` with the `exhaustAllowed`
  flag streams all remaining batches with `moreToCome`, ending in a
  trailing empty `id: 0` reply (mongod parity). Tailable / awaitData
  cursors that yield nothing fall back to ordinary `getMore` rather than
  spin the stream. A mid-stream getMore that raises unexpectedly still
  terminates the stream with a `moreToCome`-clear reply, so the client
  never sees "Server ended moreToCome unexpectedly".

### Parse-time update validation, partial-index range implication

`update` now rejects an unknown modifier (`$thismodifierdoesntexist`) at
parse time with code 9, even against an empty collection — matching
mongod, which validates the update before matching any document (the
per-document apply path would never see an unmatched update).
`createIndexes` rejects a malformed `partialFilterExpression` (a
non-document, an unknown operator, a logical operator with a non-array
argument). And a partial index whose filter uses a range operator
(`{a: {$lte: 1.5}}`) is now used when the query provably implies it (an
equality `a: 1`, or `a: {$lt: 1}`) — a sound, conservative range
implication that errs to a full scan rather than risk missing
documents; `explain` flags such a scan with `isPartial`.

#### Added

- Sound range implication for partial indexes (`$eq`/`$lt`/`$lte`/`$gt`/
  `$gte`), with `isPartial` in the explain IXSCAN stage.

#### Fixed

- `update` rejects an unknown modifier at parse time (code 9), even on
  an empty collection.
- `createIndexes` rejects a malformed `partialFilterExpression`.

### Upsert subdocument _id, and idempotent drop with write concern

Two real correctness fixes. An upsert whose filter pins `_id` to a
subdocument value (`{_id: {f: ..., f2: ...}}`) now seeds that `_id`
into the inserted document instead of generating a fresh ObjectId —
the seed extraction was skipping every dict-valued filter field to
avoid copying operator expressions (`{$gt: 5}`), but a literal
subdocument is a real equality and must be kept. And `drop` of a
non-existent collection now returns `{ok: 1}` (idempotent, as modern
mongod does) rather than `NamespaceNotFound`, which also lets an
unsatisfiable write concern surface its `writeConcernError` on the
reply.

#### Fixed

- Upsert seeds a subdocument `_id` from the filter (operator
  expressions are still correctly excluded).
- `drop` of a non-existent collection is idempotent (`{ok: 1}`) and
  honours an unsatisfiable write concern.

### Cursor min() / max() index bounds

The find command's `min` / `max` cursor options are now honoured: they
bound a hinted index scan, with `max` an exclusive upper bound and
`min` an inclusive lower bound on the index key (mongod semantics).
Bounds and documents are compared with the same direction-aware
byte-sortable key encoder the indexes use, so cross-type ordering and
per-field direction are correct. A bound whose field order doesn't
match the hinted index's key pattern is rejected with mongod's 51174.

#### Added

- Cursor `min` / `max` index-bound options on `find` (oracle-pinned
  against mongod; 51174 on a key-pattern mismatch).
- **Rust server:** change streams (R3b-a) — `aggregate` with a leading
  `$changeStream` now opens a tailable oplog cursor instead of
  rejecting, and tailable `getMore` projects insert / update / replace /
  delete events (with `documentKey`, `updateDescription`,
  `updateLookup` `fullDocument`, pre-images, and a resume token under
  `_id`). The projector runs behind a new WT-free `Storage` trait seam
  (`change_stream_poll` / `wait_for_oplog` / oplog accessors) so the
  command crate stays WiredTiger-free. Measured **+58** on the R8
  rust-server gauge (936 → 994 of 1713, zero regressions; 52 are
  `test_change_stream.py`). `awaitData` blocking, resume tokens, and
  invalidation cursor-close land in R3b-b.

### Clustered collections

The `clusteredIndex` create option is now supported. mongod uses it to
make `_id` the collection's clustering key — which is exactly
SecantusDB's WiredTiger layout already (the document table is keyed by
`_id`), so this is a metadata-and-reporting feature: the option is
validated at `create` (only `{_id: 1}` with `unique: true`, mongod's
two rejection codes), echoed in `listCollections.options.clusteredIndex`
with its `v` and defaulted name, and reported by `listIndexes` as a
single entry carrying `clustered: true` (a clustered collection has no
separate `_id_` index). Secondary indexes coexist normally.

#### Added

- `clusteredIndex` create option (`create` / `listCollections` /
  `listIndexes`), oracle-pinned against mongod.

### Matcher correctness, the validate command, and upsert _id fidelity

Continuing the honest-gauge triage, this slice fixes two genuine
correctness bugs the gauge surfaced. Embedded-document equality is now
field-order-sensitive and exact, recursively — `{size: {h: 14, w: 21}}`
matches a document only when `size` is exactly that, in that key order
(a documented mongod gotcha that Python's order-insensitive `dict ==`
got wrong). And an upsert whose resulting `_id` is `None` now reports
`did_upsert` correctly: `None` was doubling as the "no upsert"
sentinel, so a legitimate `{_id: null}` upsert looked like a no-op to
the driver.

The `validate` command is implemented — a clean, mongod-shaped
consistency report (real record and index counts; SecantusDB's
WiredTiger-backed storage has nothing to repair), including mongod's
rejection of `full` + `background` together.

#### Added

- `validate` command (collection consistency check; `full`/`background`/
  `scandata` options, full+background rejected with InvalidOptions).

#### Fixed

- Embedded-document equality is order-sensitive and exact, recursively,
  with numeric-bridged leaves (matcher correctness; both query engines —
  the Rust core already deferred Document/Array equality to Python).
- Upsert with a `None` `_id` reports `did_upsert` and the upserted `_id`
  correctly (update and findAndModify paths).
- **Rust server:** cluster-time gossip — the Rust server now attaches
  `$clusterTime` (keyless signature) and `operationTime` to every reply
  when the replica-set persona is on, matching mongod and the Python
  server (shipped in 0.5.2b19). Reads observe the clock via the new
  `secantus_storage::Storage::peek_cluster_time` without advancing it;
  standalone mode stays gossip-free. Measured +6 on the R8 rust-server
  gauge (930 → 936 of 1713, zero regressions): the `$clusterTime`-gossip,
  causal-consistency, and transaction-commit tests that read
  `operationTime`. Closes a documented Rust-server gap (backlog §7).


### The honest-gauge triage: projection, size caps, snapshot reads, and change-stream fidelity

The first honest pymongo-gauge run (94.8%) left a 64-failure triage list;
this slice clears the bulk of it. Projection gained mongod's exact
semantics for three long-standing divergences — `{_id: 1}`-only specs
are inclusion projections, dotted paths fan out over arrays (with
`{}`-skeleton preservation), and `$slice` interacts with explicit `_id`
correctly — fixed in both the Python and Rust engines with the parity
corpus extended to pin every oracle-checked case. Writes now enforce
`maxBsonObjectSize` server-side with mongod's codes and wording (10334
on insert and update-growth, 17420 on upsert).

Snapshot sessions work end-to-end: `readConcern: {level: snapshot}` is
accepted on find/aggregate/distinct (and their cursor continuations)
under the replica-set persona, with `atClusterTime` stamped on replies
for session pinning — and still rejected like a real standalone when
the persona is off. The `$$NOW` system variable landed as part of the
same path, seeded per-operation for every command's `let` scope.

Change streams got the biggest batch: events that project out the
resume token now fail with mongod's 280 `ChangeStreamFatalError` and
the `NonResumableChangeStreamError` label instead of being silently
swallowed; `fullDocument: required/whenAvailable` follow post-image
semantics (error/null when `changeStreamPreAndPostImages` is off);
`resumeAfter` rejects invalidate-event tokens (260) while `startAfter`
accepts them; `readConcern: local` on `$changeStream` is rejected;
unknown pipeline stages return mongod's 40324 at aggregate time;
pipeline-form updates emit `update` events (with `truncatedArrays`)
instead of `replace`; and `updateDescription.disambiguatedPaths` is
computed for ambiguous numeric-string field names — in both engines,
parity-pinned.

#### Added

- `$$NOW` aggregation system variable (constant per operation, all
  command `let` scopes).
- `updateDescription.disambiguatedPaths` on change-stream update
  events (Python + Rust diff engines).
- `atClusterTime` on snapshot-read replies (cursor and top-level).

#### Fixed

- Projection: `_id`-only inclusion, dotted-path array fan-out, dict
  skeletons, `$slice`+`_id` interaction (both engines).
- Server-side `maxBsonObjectSize` enforcement (10334 / 17420).
- Change streams: 280 + non-resumable label for projected-out resume
  tokens, post-image semantics for required/whenAvailable, invalidate
  tokens rejected by resumeAfter (260), local readConcern rejected,
  40324 for unknown stages at create time, pipeline updates as diff
  events, disambiguatedPaths.
- `AggregateError` can carry mongod-specific codes (40324).


### Real multi-document transactions

`commitTransaction` and `abortTransaction` were the last true stubs in
the Python server: they returned `{ok: 1}` while every operation
"inside" a driver transaction took effect immediately and could never
roll back. They're real now. Each transaction owns a dedicated
WiredTiger session — not the connection thread's, because pymongo can
legally send a transaction's statements and its retryable commit on
different pooled connections — and every statement runs with that
session swapped into the storage layer, so snapshot isolation,
read-your-own-writes, and rollback all come straight from the same
engine mongod uses. Oplog entries are buffered and flushed at commit
with one shared commit timestamp plus `lsid`/`txnNumber`, so change
streams never see uncommitted writes and transaction events carry
their session identity, exactly as in mongod.

The server-side state machine (`secantus.transactions`) pins the
spec's resolution table: statements against unknown or aborted
transactions get 251 `NoSuchTransaction` with the
`TransientTransactionError` label, committed ones get 256, stale
`txnNumber`s get 225 `TransactionTooOld`, commit is idempotent (driver
commit retries depend on it), and any failed statement aborts the
transaction server-side. Write-write conflicts between transactions
surface as statement-time 112 `WriteConflict` + transient label;
`count` inside a transaction gets mongod's 263
`OperationNotSupportedInTransaction`. Transactions idle past 60s
(`transaction_lifetime_seconds`) are reaped, `endSessions`/
`killSessions` abort their session's transaction, and `readConcern:
"snapshot"` is now accepted inside transactions (every in-transaction
read runs against the pinned WT snapshot anyway).

### The whole MongoDB CLI toolchain now runs against SecantusDB

The MongoDB Database Tools are strict Go-driver clients, and two of
them couldn't talk to SecantusDB at all: `mongostat` crashed with a Go
nil-pointer panic because `serverStatus` had no `mem` section (the
tool dereferences `mem.supported` unguarded), and `mongotop` failed
outright because the `top` command didn't exist. Both work now —
`serverStatus` reports a real resident-set size under `mem`, and `top`
returns mongod's exact per-namespace shape (counters are zero pending
per-namespace instrumentation; mongotop renders it like an idle
server).

Every connectable tool in the toolchain is pinned by an end-to-end
test in the default suite: `mongosh`, `mongodump`/`mongorestore`,
`mongoimport`/`mongoexport` (NDJSON + CSV, plus canonical-extended-JSON
type fidelity for ObjectId / datetime / Decimal128 / Int64 / Binary),
`bsondump`, `mongofiles` (GridFS put/get/list/delete against pymongo's
gridfs), and single-iteration `mongostat` / `mongotop` probes. The
Go tools also exposed two connection-lifecycle nits, now fixed: an
RST-style hang-up (how Go's pool drops connections) no longer dumps a
traceback through the catch-all handler, and a request racing
`stop()`'s socket close no longer raises `OSError` reading the server
address.

Compass gets the same treatment, headlessly: every command the GUI
issues — the connect-time instance probes, `$collStats` storage
figures, `$sample` schema analysis, `$indexStats`, both explain
verbosities, and the performance-tab polls — is pinned by tests. That
sweep caught `explain`'s `executionStats` reporting hardcoded zeroes
(Compass would render "0 documents returned" for any query); the
server now really executes the query at `executionStats` verbosity,
and aggregate-explain lifts a leading `$match` so it reports the same
IXSCAN decision the real pipeline run uses.

#### Added

- Multi-document transactions: real `commitTransaction` /
  `abortTransaction`, per-transaction WiredTiger sessions
  (`Storage.begin/use/commit/abort_user_transaction`), the
  `secantus.transactions.TransactionRegistry` state machine
  (251/256/225/50911/263/112 + `TransientTransactionError` labels,
  idempotent commit, implicit abort on a newer `txnNumber`, 60s
  lifetime reaping via `SecantusDBServer(transaction_lifetime_seconds=…)`),
  oplog buffering with a shared commit timestamp, and `lsid` /
  `txnNumber` on change-stream events for transactional writes.
  Conformance: `tests/test_transactions.py`,
  `tests/test_transaction_registry.py`, `tests/test_storage_user_txn.py`;
  divergence notes in backlog §3.4.
- Cluster-time gossip: every command reply in replica-set mode now
  carries `$clusterTime` (unsigned-cluster placeholder signature, as
  mongod without auth keys) and `operationTime`, via the non-minting
  `Storage.peek_cluster_time()`. Drivers track these per session and
  echo `readConcern.afterClusterTime` on causally consistent reads and
  transaction starts — the wire shape the transactions /
  causal-consistency unified specs assert.
- `top` command — mongod-shaped per-namespace reply (`totals` with
  `total`/`readLock`/`writeLock`/per-op `{time, count}` sections,
  RBAC `top` action granted via `clusterMonitor`); counters are zero
  (no per-namespace timing instrumentation yet, see backlog §2).
- `serverStatus.mem` section (`bits`/`resident`/`virtual`/`supported`)
  — `resident` is real (getrusage max-RSS).
- CLI-tool conformance tests: `tests/test_mongoimport_export.py`,
  `tests/test_mongofiles.py`, `tests/test_mongostat_mongotop.py`, and
  a `bsondump` dump-format test in `tests/test_mongodump_restore.py`.
- Compass headless coverage: `tests/test_compass_commands.py` pins the
  full command surface MongoDB Compass issues (instance probes,
  `$collStats`/`$sample`/`$indexStats`, explain at both verbosities,
  performance-tab polls, `atlasVersion` → CommandNotFound).
- `serverStatus` now carries a `secantus` subdocument
  (`{server: "python"|"rust", version: ...}`) on both servers —
  categorical self-identification that real `mongod` never has. The
  conformance-gauge tripwire checks it over the wire before any test
  runs, so the gauge can never again silently measure a foreign server.
- Cluster-time gossip: every reply (success or error) now carries
  `$clusterTime` (keyless signature) and `operationTime` when the
  replica-set persona is on, exactly like a real replica-set mongod;
  standalone mode stays gossip-free. Reads observe the cluster clock
  via the new `Storage.peek_cluster_time()` without advancing it.
  Clears the `startAtOperationTime` / causal-consistency bucket of the
  honest pymongo gauge (Rust-server port tracked in backlog §7).

#### Changed

- CI: the Linux and macOS test cells install mongosh + MongoDB Database
  Tools, so the CLI-tool conformance tests run continuously instead of
  skipping on runners (Windows omitted — mongosh tests skip on win32 by
  design).
- CI: all `actions/*` workflow actions bumped to their Node-24 majors
  (checkout v5, setup-python v6, upload-artifact v6, download-artifact
  v7, cache v5, setup-go v6, setup-java v5, setup-node v5) ahead of
  GitHub's June 16th 2026 forced Node 20 → 24 switch.

#### Fixed

- Arithmetic expressions (`$add` / `$subtract` / `$multiply` /
  `$divide` / `$mod`) now raise mongod's type errors instead of
  silently producing Python-flavoured results: non-numeric operands
  error with mongod's exact messages and codes (verified against a
  real mongod 8.2 oracle), `$divide`/`$mod` by zero error (codes 2 /
  16610) instead of returning null, bool operands are rejected (BSON
  arithmetic has no bool), `$add`/`$subtract` date semantics follow
  mongod (date ± millis, date − date → long, two dates in `$add` →
  16612), and Decimal128 operands widen the fold to decimal. The Rust
  engine defers all error-shaped cases to Python (parity corpus
  extended first; 536 parity tests green).
- Timeseries collections no longer enforce `_id` uniqueness, matching
  mongod (measurements are bucketed by time; `_id` is not a key there).
  Doc-table keys for timeseries rows carry a uniqueness suffix so equal
  `_id`s coexist; index entries point at the actual row key, updates and
  deletes preserve it, and the `_id` point-lookup fast path falls back
  to a collection scan for timeseries. Closes the last E11000 item from
  the honest-gauge triage.
- Aggregation-pipeline updates (`update_one(filter, [{"$set": ...}])`)
  now project as `update` change-stream events with a computed
  `updateDescription`, matching mongod. The replacement classifier
  iterated the pipeline list (whose elements are stage documents, not
  `$`-prefixed keys) and emitted a full-document oplog entry, so
  pymongo's "Test array truncation" unified spec saw `replace`.
- Stale WT read snapshots made the mutating scanners
  (`drop_collection` / `drop_database` / `rename_collection` /
  `drop_index` / `drop_all_indexes`, plus `index_sizes`) silently miss
  rows committed by other connection threads — a pinned snapshot from
  an earlier positioned cursor turned `drop` into a partial or complete
  no-op, surfacing in the pymongo gauge as drop-then-reinsert E11000
  duplicate-key errors. All six now refresh the session snapshot on
  entry, the same discipline the public read paths already had.
- `mongostat` no longer panics against SecantusDB (missing
  `serverStatus.mem`); `mongotop` no longer fails with
  `CommandNotFound`.
- `explain` with `executionStats` / `allPlansExecution` verbosity now
  really executes the query and reports actual `nReturned` /
  `totalDocsExamined` / `totalKeysExamined` / `executionTimeMillis`
  instead of hardcoded zeroes; aggregate-explain lifts a leading
  `$match` into the reported plan, matching the real pipeline run's
  index decision.
- Abrupt client resets (RST close, routine for Go-driver tools) are
  treated as normal disconnects instead of logging `unhandled error on
  connection N` tracebacks.
- Shutdown race: a request arriving while `stop()` closes the listen
  socket no longer raises `OSError: Bad file descriptor` from the
  address probe.
- **The pymongo conformance gauge was not measuring SecantusDB.**
  pymongo's test helpers freeze `DB_IP`/`DB_PORT` at conftest-import
  time, before the gauge plugin's `pytest_configure` wrote them — so
  local runs silently targeted whatever listened on `localhost:27017`
  (a real `mongod`, which produced the previous "100.0%" headline) and
  CI runs, with nothing on 27017, mass-skipped 1100+ tests. The plugin
  now starts the embedded server in `pytest_load_initial_conftests`
  (before any conftest import), aborts via tripwire if the helpers
  captured the wrong address or the target lacks the `secantus`
  marker, and the regenerated honest report shows the real number.
- The weekly `validate.yml` aggregate never opened its report PR:
  `upload-artifact@v4` strips the `docs/` parent from single-file
  artifacts, so the staging glob matched nothing and untracked new
  reports were invisible to `git diff`. Staging now fails loudly on an
  empty match and `git add --intent-to-add`s new report files.
- The gauge now runs under one xdist worker (`-n1`) with a 120s
  per-test deadline, so a hung test is recorded as a crash and the run
  continues, instead of pytest-timeout killing the whole process and
  losing the JSON report.
- Editable storage-engine rebuilds shipped stale Rust extensions: the
  CMake custom command had no dependency on the crate sources, so once
  the staged `.so` existed cargo never re-ran. The build now always
  invokes cargo (its own dependency tracking decides freshness) and
  stages with `copy_if_different`.

## [0.5.2b15] — 2026-05-22

### WT session leak fix unblocks the rust crud unified runner

SecantusDB cached a WiredTiger session per connection thread in
`threading.local()` but never released it when the thread died.
Aggressive driver pools (mongo-rust-driver's spec runners are
the canonical case) opened thousands of short-lived connections;
once cumulative connections crossed WT's 1024-session pool limit,
`hello` started failing mid-handshake with `WT_ERROR: out of
sessions`, which downstream surfaced as a checkpoint stat-error
on `WiredTigerHS.wt`. This release calls
`Storage._reset_thread_session()` in `SecantusDBServer._handle_client`'s
`finally` block, releasing the session/cursors on disconnect so
the pool stays bounded by the live connection count.

The fix also closes a small `aggregate` validation gap: `$out`
and `$merge` under `readConcern: "linearizable"` now return
`InvalidOptions (72)` to match mongod's invariant (the
`aggregate-out-readConcern` unified spec asserts the rejection).

Together these unblock `test::spec::crud::run_unified` in the
rust gauge — ~80 subtests across find / insert / update / delete
/ aggregate / countDocuments / distinct / findOne\* / replaceOne
/ bypassDocumentValidation / collation / hints / comments / let
bindings / readConcern levels / dots-and-dollars keys, running
end-to-end in ~75s. Rust gauge moves from 100 → 101 filters
passing.

#### Fixed
- WT session pool exhaustion under high connection churn: per-
  connection-thread WT session is now released on disconnect
  instead of leaking until the engine's 1024-session pool fills.
- `aggregate` with `$out` / `$merge` under `readConcern:
  "linearizable"` now errors with `InvalidOptions (72)` instead
  of silently returning an empty array.

#### Changed
- Rust conformance gauge: `test::spec::crud::run_unified` is now
  in the include list. `test::spec::collection_management::run_unified`
  and `test::spec::sessions::run_unified` remain deferred for
  separate gaps (time-series collections, snapshot read concern
  under fake replica-set topology).

## [0.5.2b14] — 2026-05-22

### Change-stream split-event implementation: real `{fragment: N, of: M}`

The `splitLargeChangeStreamEvents` opt-in previously stamped every
event with `{fragment: 1, of: 1}` regardless of size — correct from
the driver's reassembly perspective for events under 16 MB, but
wrong for events that genuinely exceed the BSON wire limit (the
typical case being an `update` with `fullDocumentBeforeChange:
required` where the pre-image plus a large `$set` value together
push the projected event past 16 MB).

This slice ships real splitting. When an event's BSON-encoded size
exceeds 16 MB, `stamp_split_event` distributes any top-level field
larger than 1 MB into its own fragment; light metadata (resume
token, operationType, clusterTime, ns, documentKey, wallTime, …)
is copied verbatim into every fragment so each is a valid change
event the driver can process independently. Fragments share the
same `_id` resume token; drivers reassemble by combining fields
across fragments with matching `_id`. The split is size-based, not
field-name-based: any heavy field qualifies (in practice
`fullDocument`, `fullDocumentBeforeChange`, and
`updateDescription.updatedFields` are the candidates).

Two opt-in paths now both light up the producer flag: the original
`$changeStream: {splitLargeChangeStreamEvents: true}` spec field
plus the pipeline-stage form `[{$changeStreamSplitLargeEvent: {}}]`
that the rust / node / java drivers use from their high-level
`watch()` APIs. Either signals to the producer that fragmentation
should run.

mongo-rust-driver's `test::change_stream::split_large_event` —
which constructs a 10 MB pre-image + 10 MB update value and
asserts `events[0].splitEvent == {fragment: 1, of: 2}` and
`events[1].splitEvent == {fragment: 2, of: 2}` — now passes end-
to-end. The rust gauge moves from 92 → 93 (still 100%).

#### Added

- `src/secantus/aggregate.py`: `$changeStreamSplitLargeEvent`
  registered in `_STAGES` as a pass-through marker. The stage
  itself is a no-op in the pipeline (real splitting happens
  upstream at event-projection time); accepted spec is `{}`.
- `src/secantus/changestreams.py`:
  - `_HEAVY_FIELD_BYTES = 1 MB` and `_SPLIT_THRESHOLD_BYTES = 16 MB`.
  - `stamp_split_event(event) -> list[dict]` rewritten to compute
    the event's BSON size, identify heavy top-level fields by
    per-field encoding, and emit one fragment per heavy field
    with light metadata duplicated. Returns one event (no split)
    when the original is under 16 MB.
- `src/secantus/commands.py`: change-stream aggregate handler
  detects the `$changeStreamSplitLargeEvent` pipeline stage and
  sets `cs_spec.split_large_events = True` so the producer
  fragments on that opt-in path too. Producer call sites
  changed from `events.append(stamp_split_event(ev))` to
  `events.extend(stamp_split_event(ev))`.
- `tests/test_change_stream_split_stage.py` (5 tests):
  pipeline parses cleanly; bad-spec rejected standalone; stage
  works outside change-stream context (no-op pass-through);
  10 MB pre-image + 10 MB `$set` value produces two fragments
  with correct `{fragment: N, of: 2}` envelopes and shared
  resume token, heavy fields distributed one per fragment;
  small event with opt-in still produces single
  `{fragment: 1, of: 1}` fragment.

#### Changed

- `rust_validation/include_paths.py` adds
  `test::change_stream::split_large_event` to `INCLUDE` (rust
  gauge 92 → 93). The previous EXCLUDED entry's rationale is
  removed.

### Point lookups by `_id` stop scanning the whole collection

Every MongoDB collection has an `_id` index, and looking a document up
by its `_id` is the single most common read an application makes. In
SecantusDB that lookup was quietly walking the entire collection: the
`_id_` index is virtual — the documents table is itself keyed by the
encoded `_id`, so there's no separate entries table for it — and the
query planner's index pickers only ever consulted the stored secondary
indexes. With nothing matching `_id`, every `find({_id: …})` fell back
to a COLLSCAN that got linearly slower as the collection grew.

`find`, `findOne`, `updateOne`, and `deleteOne` filtered on `_id` now
take a direct primary-key point lookup on the documents table instead.
On a 5,000-document collection that turns a 45 ms read into a 0.6 ms
read — about 74× faster — and the gap widens with collection size.
`explain` reports the lookup honestly as an `IXSCAN` on the `_id_`
index. Equality (`{_id: x}`), `{_id: {$eq: x}}`, and `{_id: {$in: […]}}`
are all accelerated; range, regex, and multi-field filters keep their
existing routing. The cross-numeric `_id` collision (`1 == 1.0 ==
Decimal128("1")`) is preserved because the fast path encodes the query
value with the same `encode_value` used for the stored key.

#### Fixed

- `find` / `findAndModify` / single-document `update` / `delete`
  filtered on `_id` equality (`{_id: v}`, `{_id: {$eq: v}}`,
  `{_id: {$in: [...]}}`) now do an O(1) primary-key point lookup on the
  documents table instead of a COLLSCAN, and `explain` reports `IXSCAN`
  on the `_id_` index. Discovered with the new `bench/rw_harness.py`
  concurrent read/write validator, whose interleaved `_id` read-backs
  collapsed throughput on growing collections.

## [0.5.2b7] — 2026-05-21

### Rust driver gauge — 6th conformance gauge alongside the rest

mongo-rust-driver is now the 6th driver gauge alongside pymongo / go
/ node / java / ruby. The runner spawns SecantusDB on an ephemeral
port and runs ``cargo test --lib -p mongodb`` against a curated
include set with ``MONGODB_URI`` explicitly overridden in the
subprocess env — the rust driver's fallback chain
(``$MONGODB_URI`` → ``~/.mongodb_uri`` → ``localhost:27017``) is
short-circuited at the first step so a stray ambient URI in the
user's shell can't route the gauge at a real mongod. A
belt-and-braces ``hello.setName == "secantus"`` probe at runner
start adds a second layer of confirmation.

Initial baseline: 12 curated handshake + single-collection CRUD
filters expand to 24 actual test runs (libtest substring matching
fans ``test::coll::find`` out across ``find_allow_disk_use`` etc.).
The first cut surfaced two real conformance gaps; both fixed in the
same release:

* ``listDatabases`` now populates ``sizeOnDisk`` per database (sum
  of bson-encoded doc bytes across the db's collections — same
  accounting ``collStats`` / ``dbStats`` use). ``empty`` is derived
  from the size (``size == 0``). ``totalSize`` reports the actual
  sum across all dbs. Previously every entry carried a placeholder
  ``sizeOnDisk: 0`` and ``empty: false``.
* ``hello.client`` subdoc captured per connection in the registry
  and surfaced back via ``currentOp`` as ``clientMetadata``. Drivers
  use it to identify their own connections in admin tooling — they
  send the subdoc on handshake and expect to read it back. Previously
  we threw the subdoc away on hello and ``currentOp`` emitted no
  ``clientMetadata`` field.

After the fixes the rust gauge runs **24/24 (100%)**.

#### Added

- ``rust_validation/`` package — ``__init__.py`` /
  ``include_paths.py`` / ``runner.py`` / ``generate_report.py``,
  mirrors the ``ruby_validation/`` shape.
- ``vendor/mongo-rust-driver`` submodule (7th vendored driver).
- ``invoke validate-rust`` task; ``validate-all`` GAUGES extended
  with the 6th entry.
- ``.github/workflows/validate.yml`` matrix entry for rust;
  toolchain via ``dtolnay/rust-toolchain@stable``; cargo cache key
  on ``vendor/mongo-rust-driver/Cargo.lock``.
- ``validation_summary`` integration — ``_collect_rust``,
  ``PANEL_PROSE`` entry, stale "pending" marker removed.
- ``docs/validation-report-rust.md`` (new) + toctree entry +
  index.md prose update referencing all six drivers.
- ``tests/test_list_databases_size.py`` (4 tests): populated db
  has non-zero ``sizeOnDisk`` + ``empty: false``; ``totalSize``
  sums per-db sizes; ``nameOnly`` skips the size walk; ``filter``
  scopes against the full descriptor.
- ``tests/test_hello_client_metadata.py`` (2 tests): pymongo's
  driver / OS / appname metadata round-trips through hello →
  currentOp; clientMetadata is a dict shape when present.

#### Changed

- ``commands._list_databases``: computes ``sizeOnDisk`` per db as
  ``sum(collection_data_size(...) for coll in list_collections)``;
  ``empty`` derived from size; ``totalSize`` is real.
- ``commands._hello``: captures ``doc.get("client")`` and stashes
  via ``ctx.connections.set_client_metadata(...)``.
- ``commands._current_op``: emits ``clientMetadata`` on each
  in-progress op when the connection's registry entry has it.
- ``connreg.ConnInfo`` grows ``client_metadata: dict | None``;
  ``ConnectionRegistry.set_client_metadata(conn_id, metadata)``
  added; ``get()`` and ``snapshot()`` thread the new field
  through their fresh-copy semantics.

## [0.5.2b5] — 2026-05-21

### `$setWindowFields` rank functions — `$rank` / `$denseRank` / `$documentNumber`

Closes one of the explicit deferred surfaces from the b35
`$setWindowFields` minimum-viable subset. Driver test suites probe
all three regularly; the previous wire-level response was an
explicit "rank functions and time-series operators are not yet
implemented" `AggregateError`.

The three functions share one linear walk per partition. They sit
in `output: {<field>: {$rank: {}}}` alongside the accumulator
functions but evaluate differently — no window argument (mongod
rejects it), no function argument (the spec is just `{$rank: {}}`),
and the value is computed once per partition slot rather than
rolled up over a windowed subset.

* `$documentNumber` — 1-indexed position within the partition.
  Independent of ties; happy with or without `sortBy`.
* `$rank` — 1-indexed position with **gaps** after ties: tied rows
  share the lower rank, next non-tied row jumps by the number of
  ties (`[10, 20, 20, 30]` → `[1, 2, 2, 4]`). Requires `sortBy`.
* `$denseRank` — 1-indexed position **without gaps**: tied rows
  share, next row is +1 (`[10, 20, 20, 30]` → `[1, 2, 2, 3]`).
  Requires `sortBy`.

Tie detection is sort-key tuple equality: compound `sortBy` specs
work uniformly. Rank counters reset at every partition boundary,
same as the accumulator functions.

#### Added

- `src/secantus/aggregate.py`: `_RANK_FUNCS` frozenset; the
  validation branch in `_stage_set_window_fields` recognises the
  three rank ops, rejects `window` / non-empty arg, and requires
  `sortBy` for `$rank` / `$denseRank`. The per-row loop branches:
  rank functions look up a precomputed array, accumulators take
  the existing windowed path.
- `_compute_rank_state` helper does one linear walk over each
  partition's sort-key tuples and emits per-slot vectors for
  whichever of the three functions are referenced. `_sort_key_values`
  extracts the tuple the tie comparison runs on.
- `tests/test_window_rank_functions.py` (13 new tests) — covers
  `$documentNumber` with and without sort, per-partition reset,
  `$rank` gaps with ties, `$rank == $documentNumber` without ties,
  compound sort tie detection, `$denseRank` no-gap semantics, all
  three together in one stage, partition-resets, plus four
  validation tests (window rejected, sortBy required for `$rank` /
  `$denseRank`, non-empty arg rejected).

#### Changed

- `_stage_set_window_fields` docstring rewritten to document the
  rank-function surface.
- `tests/test_set_window_fields.py`: the b35 placeholder test
  `test_unsupported_rank_function_raises` is replaced by
  `test_unsupported_time_series_function_raises`, which now probes
  with `$derivative` to keep the deferred-surface guard alive.

### `apiStrict: true` rejects `distinct` (narrow command-name gate)

The Stable API v1 contract rejects a list of commands when
`apiStrict: true` is set. SecantusDB already rejected non-v1
aggregation **stages** inside `aggregate` pipelines (lights up
mongo-java-driver's `versioned-api/aggregate on database` test
that probes with `$listLocalSessions`). The matching command-name
gate had been intentionally left off in a previous attempt: a
broader whitelist invert reportedly caused 6 cascade failures via
`MongoConnectionPoolClearedException`.

A focused Java-gauge run with a narrow gate
(`_API_V1_REJECTED_BY_NAME = {"distinct"}`) tells a different
story. Rejecting only `distinct` produces **+1 pass** for the
canary `crud-api-version-1-strict.yml` `distinct appends declared
API version` test and **zero** new failures across the 900-test
mongo-java-driver suite — no pool-clear symptoms anywhere in the
JUnit XML. The cascade the previous attempt observed was not
pool-clear semantics; it was the broader invert also rejecting
`count` (used internally by `estimatedDocumentCount`) and other
handshake-adjacent internal commands. The narrow gate sidesteps
that mechanism entirely.

#### Added

- `src/secantus/commands.py`: `_API_V1_REJECTED_BY_NAME`
  frozenset (one entry: `distinct`); the `dispatch` apiStrict
  block grew a command-name check that runs before the
  aggregation-stage check. The rejection's `errmsg` matches
  mongod's `"Provided command distinct is not in API Version 1"`
  so the unified test runner's `errorContains` assertion fires
  cleanly.
- `tests/test_api_strict.py` (5 new tests): `distinct` rejected
  under `apiStrict: true` with code 323; `distinct` allowed
  without `apiStrict`; `count` still allowed under `apiStrict`
  (the cascade-avoidance check); `find` still allowed; `aggregate`
  with a v1 stage still allowed (gates compose).

#### Changed

- Backlog §5 entry on `apiStrict` pool-clear struck through with
  the empirical resolution path. The previous theory turned out
  to be wrong about the mechanism — narrow rejection works.

### Pymongo gauge: +80 passing tests from five newly-includable files

Cross-gauge audit of currently-excluded test files against the work
shipped in this development cycle (0.5.2b1 + the rank-functions
and apiStrict slices above) identified five pymongo test files
that pass cleanly now and had been excluded purely because the
supporting features hadn't shipped. Adding them to
`pymongo_validation/include_paths.py` bumps the gauge from **959 →
1039 passing** with zero new failures, +25 new skips (genuine
feature gaps the suite self-skips on), overall pass rate stays at
100%.

* `test_collation.py` (16 new tests) — unlocked by per-index
  collation work (single-field, compound, sort acceleration).
* `test_versioned_api.py` (4 tests) + `test_versioned_api_integration.py`
  (36 tests) — unlocked by the apiStrict aggregation-stage gate
  and the new `distinct` command-name gate.
* `test_command_logging.py` (20 tests) + `test_logger.py` (4 tests)
  — command monitoring / logging format conformance; no
  SecantusDB-specific blocker.

The audit also confirmed no flip-worthy candidates in the go /
node / java / ruby gauges — every remaining exclusion in those
gauges is a feature genuinely out of scope (replica sets,
transactions, encryption, text indexes, GridFS, time-series,
etc.).

#### Changed

- `pymongo_validation/include_paths.py` — five test files added
  to `INCLUDE`. Inline comments name the slice that unlocked each.

## [0.5.2b1] — 2026-05-20

### MONGODB-X509 auth — cert subject DN as the username

The natural sequel to the b22 mTLS slice. mTLS gives you a
transport-layer "approved client" gate; MONGODB-X509 turns the
client cert's subject DN into the user identity directly, no SCRAM
step. Same flow MongoDB Atlas X509 deployments use: create the user
on `$external` with `mechanisms: ["MONGODB-X509"]` and the cert DN
as the username, connect with
`?authMechanism=MONGODB-X509&authSource=$external`, the server
matches the DN from the verified cert against the user record. No
password to rotate, no SCRAM round-trip, no shared secret on disk.

Mixed mechanisms work too — a user record can carry both
`SCRAM-SHA-256` and `MONGODB-X509` in `mechanisms` for migration or
to keep a SCRAM fallback. The driver picks per-connection from
`saslSupportedMechs`.

Closes the "transport-layer gate only" caveat the production +
configuration docs called out when mTLS shipped; documentation
updated to point at the worked X509 example as the alternative to
SCRAM-on-top.

#### Added

- `secantus.auth.MONGODB_X509` constant, `X509_CREDENTIAL_MARKER`
  for the user record's `credentials` doc (no password to hash —
  the credential IS the cert), and
  `secantus.auth.subject_dn_from_peercert()` which converts
  Python's `ssl.SSLSocket.getpeercert()` tuple-of-tuples into the
  mongod-style RFC 4514 DN string (short attribute names,
  most-specific-first, special-char escaping).
- `CommandContext.peer_cert_dn` — server captures the verified
  client cert's DN once per connection (right after the TLS
  handshake in `_handle_client`), replays it into every
  `CommandContext` so the auth handlers can read it.
- `_sasl_start_x509` and the legacy `authenticate` command handler
  — pymongo / Java / Go / Node all use the legacy command path for
  X509, not `saslStart`. Both are wired up and refuse cleanly on
  plaintext connections / non-X509 users / payload-DN mismatch.
- `createUser` accepts `mechanisms=["MONGODB-X509"]` with no
  password (cert IS the credential). Mixed
  `["SCRAM-SHA-256", "MONGODB-X509"]` works too — SCRAM creds are
  derived from `pwd`, X509 marker is written alongside.
- `tests/test_x509_auth.py` — 9 tests: DN extraction unit tests
  (reversal, short names, escaping, empty), end-to-end happy path
  via pymongo, refused-with-no-matching-user, refused-for-SCRAM-only
  user, SCRAM still works on mTLS-required server, X509 refused on
  plaintext connection.

#### Changed

- `saslSupportedMechs` now includes `MONGODB-X509` when a user has
  that mechanism in its `credentials` doc. SCRAM is still listed
  first when both are available (drivers pick the strongest).
- `_PRE_AUTH_COMMANDS` includes `authenticate` so the legacy X509
  command path bypasses the require-auth gate (same as
  `saslStart` / `saslContinue` already did for SCRAM).
- `docs/authentication.md` — new MONGODB-X509 section with the
  provisioning + connection examples; the stale "what's not here
  yet" list rewritten (RBAC, updateUser, grantRolesToUser, TLS,
  SCRAM-SHA-1 all shipped slices ago and shouldn't have been
  listed as gaps).
- `docs/production.md` + `docs/configuration.md` — mTLS sections
  now offer two routes (SCRAM-on-top vs MONGODB-X509) instead of
  the "transport-layer only, MONGODB-X509 is a follow-on" caveat.

### Per-index collation — case- and accent-insensitive lookups at IXSCAN

The last entry on the compatibility doc's "Deferred" list is gone.
Before this slice, the per-query collation infrastructure already
honoured `collation` for `find` / `count` / `distinct` /
`findAndModify` via `matches()` — but any query that carried a
`collation` argument fell through to COLLSCAN by design, because
index entries were written in raw BSON codepoint order. The
storage-layer comment said as much: "we don't support per-index
collation yet, so the safe path is always-COLLSCAN-when-collation."

That comment is gone. `createIndexes` with a `collation` option
now writes index entries under collation-normalised bytes —
strings that compare-equal under the collation produce the same
key, so a query carrying a matching `collation` hits the same row
at IXSCAN. Strength 1/2/3 + `caseLevel` are supported;
`numericOrdering` still falls back to COLLSCAN (would need a
length-prefixed digit-run encoding to stay byte-sortable, deferred
until a workload needs it).

Two indexes on the same field with different collations are
allowed — the picker walks every candidate and uses the one whose
collation exactly matches the query's. Useful for collections that
mix case-sensitive and case-insensitive lookups against the same
column. Unique indexes with a collation enforce uniqueness
*under* the collation: two docs differing only by case collide
against a `strength: 2` unique index. Only the single-field
equality / range / `$in` picker threads collation through today;
multi-field filters combined with a collation still fall back to
COLLSCAN. Worth widening case-by-case when a workload needs it.

#### Added

- `sortkey.encode_value(value, *, collation=None)`,
  `encode_value_directed`, `encode_compound`, and the bound
  helpers (`gt_bound` / `gte_bound` / `lt_bound` / `lte_bound`) all
  take an optional `collation` kwarg. When set and the value is a
  string, normalisation runs through
  `secantus.collation.normalize_for_index_bytes` before encoding,
  so equal-under-collation strings produce equal bytes.
- `Collation.supports_index_encoding` — True for strength 1/2/3 +
  `caseLevel`, False for `numericOrdering`. The picker treats
  numericOrdering as "no index available for this collation."
- `secantus.collation.normalize_for_index_bytes(s, collation)` —
  bytes form of the collation-normalised string (strips accents
  for strength 1, casefolds for strength ≤ 2, UTF-8 encodes).
- `_parse_index_collation` helper in `storage.py` — reads an
  index's stored collation option blob into a `Collation`,
  returning `None` for collations that don't support index
  encoding.
- `tests/test_per_index_collation.py` — 11 tests covering routing
  (matching collation → IXSCAN, mismatch → COLLSCAN, no-collation
  query against collation-having index → COLLSCAN), correctness on
  equality / range / `$in` / `update_one`, `numericOrdering`
  fallback, unique-index-under-collation, and two indexes on the
  same field with different collations.

#### Changed

- `_index_key` / `_index_key_variants` (the byte-key builders for
  index writes) accept a `collation` kwarg; the storage writers
  load it from the index's stored options and pass it through.
- `_find_leading_field_index` + `_pick_index_for_filter` +
  `_try_index_lookup` + `_try_index_id_keys` thread a `collation`
  kwarg. Indexes whose stored collation doesn't exactly equal the
  query's are skipped — the caller falls back to COLLSCAN, which
  is the safe semantics. `_pick_compound_eq_index` /
  `_pick_compound_range_index` skip collation-having indexes
  entirely; compound pickers don't yet support collation, and
  picking a collation-having index for a no-collation multi-field
  filter would return wrong rows.
- `explain_plan` takes a `collation` kwarg, and the `explain`
  command extracts it from the wrapped command. Mismatched
  collations report COLLSCAN in `winningPlan`; matched ones
  report `IXSCAN` with the index name.
- `find_matching`'s "if collation present, always COLLSCAN" gate
  has been rewritten — now tries the collation-aware index path
  first, falls back to COLLSCAN only when no matching index
  exists.
- `docs/compatibility.md` field-options table: `collation` is now
  Honoured rather than Accepted-but-ignored. The Deferred list is
  now empty.
- `docs/indexes.md`: new "Per-index collation" section with
  examples and rules; the "What's still missing" list updated to
  call out compound-index collation as the next widening.
- `tasks/backlog.md` §2: the per-index-collation stopgap entry is
  struck through with a one-line summary of what shipped and the
  remaining compound-index limitation.

### Compound-index collation — multi-field filters light up under matching collation

The b25 per-index collation slice closed the single-field path
but left the compound pickers
(`_pick_compound_eq_index` / `_pick_compound_range_index`) skipping
any collation-having index — a multi-field filter combined with a
`collation` argument fell back to COLLSCAN even when a compound
collation index could have served it. This slice closes that gap.

Both compound pickers now thread `collation` through and gate by
exact match against each index's stored collation, the same rule
the single-field path already used. The lookup builders thread
collation into every `encode_value_directed` call (leading-equality
prefix bytes and the trailing operator's bound bytes), so the
lookup hits the same byte rows the index-write path produced.
Strength 1/2/3 + `caseLevel` apply uniformly across single- and
compound-field indexes; `numericOrdering` still falls back to
COLLSCAN at every level. The unique-probe path now reads the
index's stored collation too, so a unique compound index with
`{strength: 2}` correctly rejects a second insert whose values
collide under the collation.

After this slice, every CRUD pattern that the single-field
collation path covers — equality / range / `$in` / `update` /
unique enforcement — covers under compound indexes too.

#### Changed

- `_pick_compound_eq_index` + `_try_compound_eq_id_keys` thread
  `collation` through; the compound-eq lookup builds the prefix
  bytes under the same collation as the index.
- `_pick_compound_range_index` + `_try_compound_range_id_keys`
  thread `collation` through; the trailing operator's `$eq` /
  `$in` / `$gt` / `$gte` / `$lt` / `$lte` bounds are all encoded
  under the collation.
- `_try_index_id_keys` no longer short-circuits compound pickers
  when `collation` is set — they're called with the collation kwarg
  and use the exact-match gate.
- `_pick_index_for_filter` (the explain planner) mirrors the same
  threading, so `explain` reports `IXSCAN` for collation-matching
  multi-field queries.
- `_unique_conflict` reads each index's stored collation via
  `_parse_index_collation` and threads it to `_index_key`, so the
  unique probe collides on byte-equal canonical keys (the bug
  that let `("Alice","Boston")` and `("ALICE","BOSTON")` both land
  in a unique strength-2 compound index).
- `docs/indexes.md` "Per-index collation" section rewritten to
  cover the compound case with examples; "What's still missing"
  drops the compound-collation entry.
- `tests/test_compound_index_collation.py` (10 new tests): compound
  bare-eq IXSCAN under matching collation, leading-prefix-only
  scan, mismatch → COLLSCAN, no-collation-vs-collation index
  selection across two indexes on the same fields, compound
  prefix + trailing-operator (`$gt`, `$in`) under collation,
  update via compound collation index, unique compound collation
  enforcement, `numericOrdering` fallback.

### Sort acceleration with collation — index walk replaces Python sort

The third collation slice closes a quieter gap left by the
preceding two. The b25 + b27 slices wired up filter-side
collation routing — equality / range / `$in` / compound bare-eq /
compound prefix + trailing-operator all light up at IXSCAN when
the query's `collation` matches an index's stored collation. But
the sort path stayed on COLLSCAN + Python `sort_docs`: any query
carrying a `collation` argument fell into a single branch that
never tried sort acceleration, even when an index whose collation
matched the query's would have given the requested order for free
just by walking it.

That branch is gone. The collation and non-collation paths through
`find_matching` are now unified, and every sort-picker call
(`_find_leading_field_index` for single-field sorts,
`_compound_index_for_sort` for multi-field) threads
`collation_obj` through with the same exact-match gate as the
filter side. A `find().sort("name", 1).collation({strength: 2})`
walks a `{name: 1}` strength-2 collation index forward; `-1` walks
it backward; multi-field sorts that exactly match (or fully
invert) a compound collation index's key spec walk it forward or
backward respectively, and no Python sort runs in either case.
The same gate keeps no-collation sorts off collation indexes
(walking would give the wrong order) and vice versa.

After this slice the collation domain is structurally complete:
every CRUD pattern that hits an index without collation — filter
lookup, range, `$in`, multi-field filter, sort, compound sort,
unique enforcement — hits the index when a matching collation is
in play, and falls back to COLLSCAN + `matches()` + `sort_docs`
when no matching index exists.

#### Changed

- `find_matching`'s `elif collation_obj is not None: ...` branch
  removed; the no-collation branch's sort logic now runs for both
  cases, with `collation=collation_obj` (which is `None` when no
  collation set) threaded through every picker call. Single-field
  sort + filter on the sort field, single-field sort with empty
  filter, and multi-field sort (compound key match) all
  collation-gate.
- `_compound_index_for_sort` takes an optional `collation` kwarg
  and gates by exact match against each index's stored collation
  (same rule as `_find_leading_field_index` and the compound
  filter pickers). Multikey indexes are still excluded from
  sort acceleration regardless of collation.
- `explain_plan` mirrors the threading: `_find_leading_field_index`
  and `_compound_index_for_sort` both receive `collation=collation_obj`,
  so `explain` reports IXSCAN with the right direction for
  collation-matching sort queries and COLLSCAN otherwise.
- `docs/indexes.md` "Per-index collation" section grows a "sort
  acceleration honours the same gate" subsection with worked
  forward / backward / mismatch examples.
- `tests/test_sort_with_collation.py` (8 new tests): single-field
  ASC + DESC sort with matching collation walks index forward /
  backward; no-collation sort against collation index → COLLSCAN;
  strength-2 index + strength-3 query → COLLSCAN; filter on sort
  field with matching collation hits index in order; multi-field
  sort that matches a compound collation index walks forward; the
  full-inverse sort walks backward; multi-field mismatch falls
  back to Python sort.

### `$type: "int"` / `"long"` distinguishes by BSON type tag, not value range

A quieter long-standing bug in the `$type` query operator. The
`_TYPE_PREDS` table used a Python value-range check
(`-2**31 <= v <= 2**31 - 1`) to distinguish int32 from int64. A
doc inserted as `Int64(5)` — value fits in int32 numerically, but
its BSON tag is int64 — was matched by `$type: "int"` instead of
`$type: "long"`, contradicting mongod.

pymongo's BSON decoder already preserves the int32/int64
distinction by class: int32 round-trips as plain `int`, int64
round-trips as `bson.Int64` (a subclass of `int`). The fix keys
on `isinstance(v, bson.Int64)` for "long" and
`isinstance(v, int) and not isinstance(v, (bool, Int64))` for
"int" — type-tag-faithful, no value-range arithmetic.

`$convert: {to: "long"}` had a paired bug: it returned a plain
`int` so its output couldn't be matched by `$type: "long"` on a
downstream `$match`. Now wraps the result in `Int64` for code 18
(int64); `to: "int"` (code 16) still returns plain `int`.

#### Changed

- `src/secantus/query.py`: replaced `_is_bson_int(... ranged=...)`
  + `_INT32_RANGE` with three named predicates (`_is_int32`,
  `_is_int64`, `_is_bson_number`). `_TYPE_PREDS` entries for
  `int` / `16` / `long` / `18` / `number` now route through them.
- `src/secantus/expressions.py`: `_convert_value` code 18 path
  wraps its result in `Int64` (codes 16 and 18 share the input
  coercion logic but the wrapper diverges).
- `tests/test_type_int32_int64.py` (8 new tests): `Int64(5)` →
  `$type: "long"` (not `int`); plain `int(5)` → `$type: "int"`;
  large int (`2**40`) round-trips as Int64 → `long`;
  `$type: "number"` accepts both; numeric `$type` codes (16, 18)
  agree with their string aliases; array-form `$type` matches
  either; `$convert: {to: "long"}` output matches `$type: "long"`;
  `$convert: {to: "int"}` output matches `$type: "int"`.

### `$unionWith` aggregation stage

A v1 stable-API stage that wasn't yet wired up. `$unionWith`
concatenates docs from a second collection — optionally filtered
through a sub-pipeline — onto the current pipeline's input. Driver
test suites probe it routinely; the prior wire-level response was
a generic "unsupported aggregation stage" error.

Both spec shapes ship:

* Shorthand: `{$unionWith: "<coll>"}`
* Full form: `{$unionWith: {coll: "<coll>", pipeline: [...]}}`

Outer docs land first, then the union docs in the order the
sub-pipeline produced them. No deduplication — duplicates across
the boundary survive, matching mongod. The sub-pipeline runs in a
fresh :class:`PipelineContext`; outer `$lookup let` variables are
deliberately not visible (mongod doesn't accept a `let` field on
`$unionWith`). Chained `$unionWith` stages accumulate; downstream
`$sort` / `$group` / `$count` / `$limit` see the combined set.

A non-existent target collection is treated as empty (mongod's
behaviour). Bad specs (non-string shorthand, missing `coll`,
non-array `pipeline`) surface as `AggregateError` to the client.

#### Added

- `src/secantus/aggregate.py`: `_stage_union_with` handler;
  wired into `_STAGES` next to `$geoNear`. ~30 LOC + docstring.
- `tests/test_union_with.py` (11 new tests): shorthand form;
  full form with and without sub-pipeline; outer-first ordering;
  no-dedup across boundary; chained `$unionWith`; downstream
  `$group` / `$sort+$limit`; missing collection treated as empty;
  empty outer + non-empty union; bad-spec rejection (numeric
  spec, missing `coll`, non-array `pipeline`).
- `docs/aggregation.md` stages table grows a row.

### `admin.system.users` is a synthetic read-only view onto the user store

Credentials live in a dedicated WT table (`secantus_users`) that
`createUser` / `updateUser` / `dropUser` / `usersInfo` own. But
`find` / `aggregate` / `count` against `admin.system.users` —
mongod's canonical user-storage namespace — searched the empty
regular doc table and returned nothing. Tools and a few driver
tests that introspect the user list via `db.system.users.find()`
saw an empty collection on SecantusDB even after a `createUser`
landed.

This slice mirrors the oplog pattern (`local.oplog.rs` is a
synthetic view onto `secantus_oplog`). `admin.system.users` is now
read-only-surfaced: `find` / `aggregate` / `count` route through
`_find_system_users` / `_count_system_users`, which scan the user
table on a fresh WT session for cross-thread visibility and apply
the standard filter / sort / skip / limit / projection /
collation pipeline against the decoded records.

The stored records already carry the mongod-shaped fields
(`_id` = `<db>.<user>`, `user`, `db`, `credentials`, `roles`,
`mechanisms`), so the view requires no schema synthesis. Users
created against any database all surface under
`admin.system.users` (matching mongod — every user record lives
in `admin.system.users` regardless of its auth db, and the
per-record `db` field names the auth database). Querying any
other db's `system.users` returns empty rows (also mongod's
behaviour).

Writes are rejected with code 13 (`Unauthorized`) and a clear
errmsg pointing users at `createUser` / `updateUser` / `dropUser`.
The existing `_reject_oplog_rs_write` helper grew a clause for
`admin.system.users` — it was already wired into every write
command (`insert` / `update` / `delete` / `findAndModify` / `drop`
/ `create` / `createIndexes`) so the rejection lands everywhere
implicitly. Function name kept (`_reject_oplog_rs_write`) for
churn reasons, with the docstring updated to cover both views.

#### Added

- `storage._is_system_users` / `_scan_user_records` /
  `_find_system_users` / `_count_system_users` — the synthetic
  view helpers, modelled directly on the oplog view's pattern.
- `storage.find_matching` + `count_matching` route through the
  new helpers when `(db, coll) == ("admin", "system.users")`.
- `tests/test_system_users_view.py` (13 new tests): find /
  count / projection / aggregate against the view; users created
  across multiple databases all visible; filter on `db` field;
  other-db `system.users` is empty; write rejection on insert /
  update / delete / drop with code 13; `dropUser` /
  `updateUser` mutations reflected in the view.

#### Changed

- `commands._reject_oplog_rs_write` grew a second case for
  `admin.system.users`. Docstring rewritten to cover both views.
  Existing call sites pick up the new behaviour with no further
  edits.

### `$redact` aggregation stage

The largest v1 stable-API aggregation stage still missing. `$redact`
implements content-based document and sub-document pruning — the
pipeline analogue of mongod's field-level access control. The
stage's expression evaluates against each (sub-)doc and returns one
of three sentinel strings; the result drives include / exclude /
recurse behaviour. Driver test suites probe it routinely.

* `"$$KEEP"` — include the sub-doc as-is, no recursion into nested
  sub-docs. Useful for "trusted" sub-docs whose interior shouldn't
  be re-evaluated.
* `"$$PRUNE"` — drop the sub-doc. At the top level the doc leaves
  the pipeline entirely; in a nested context the sub-doc is removed
  from its parent field, or from its array element slot (with the
  surrounding array preserved).
* `"$$DESCEND"` — recurse into every dict-valued field and every
  dict-valued list element. Non-dict scalars and non-dict list
  elements pass through unchanged.

The three sentinels are wired into the expression evaluator as
system variables (alongside `$$ROOT`, `$$CURRENT`, `$$REMOVE`);
their resolved value is the literal `"$$NAME"` string the stage
handler dispatches on. Returning anything else from the expression
raises `AggregateError` — matches mongod.

The stage uses the standard `$cond` / `$switch` / `$let` /
`$ifNull` plumbing that the rest of the expression engine already
provides, so the typical pipeline shape works straight out:

```python
[{"$redact": {
    "$cond": {
        "if": {"$eq": [{"$ifNull": ["$classified", False]}, True]},
        "then": "$$PRUNE",
        "else": "$$DESCEND",
    },
}}]
```

#### Added

- `src/secantus/aggregate.py`: `_stage_redact` handler + private
  `_redact_subdoc` / `_redact_descend` recursive helpers, wired
  into `_STAGES` next to `$unionWith`. The `_redact_descend` walker
  preserves non-dict scalars and non-dict list elements; pruned
  sub-docs are dropped from their parent field or array.
- `src/secantus/expressions.py`: `_resolve_var` recognises
  `$$KEEP` / `$$PRUNE` / `$$DESCEND` and returns the literal
  `"$$NAME"` string — same pattern as `$$REMOVE` for `$setField`.
- `tests/test_redact.py` (11 new tests): unconditional KEEP and
  PRUNE; conditional KEEP-vs-PRUNE access-control canon; DESCEND
  with nested sub-doc pruning; DESCEND into arrays of sub-docs
  with non-dict elements preserved; multi-level deep recursion;
  KEEP short-circuits descent (nested PRUNE never fires); chained
  with `$match`; non-sentinel return rejected; null / empty
  expression rejected; array-element KEEP preserves nested
  sub-docs unchanged.

### `admin.system.version` returns the auth-schema doc

The companion to the b31 `admin.system.users` view. Some
user-management tools (and a handful of driver tests) read
`admin.system.version.find({_id: "authSchema"})` on startup to gate
which user-management features they offer; pre-slice that namespace
was empty and tools either skipped features or assumed the lowest
schema version.

The view returns one hard-coded doc:

```python
{"_id": "authSchema", "currentVersion": 5}
```

`currentVersion: 5` is the SCRAM-SHA-256 baseline (MongoDB 4.0+),
which is what SecantusDB actually implements — so the answer is
honest, not just placating. Other databases' `system.version` still
returns empty. Writes are rejected with code 13 (`Unauthorized`)
via the same `_reject_oplog_rs_write` helper that gates
`admin.system.users` and `local.oplog.rs`.

#### Added

- `storage._is_system_version` / `_system_version_docs` /
  `_find_system_version` / `_count_system_version` — same pattern
  as the b31 `admin.system.users` view; the doc set is fixed at
  one entry rather than scanned from a table.
- `storage.find_matching` + `count_matching` route through the
  new helpers when `(db, coll) == ("admin", "system.version")`.
- `commands._reject_oplog_rs_write` grew a third case for
  `admin.system.version`; existing call sites pick up the
  rejection with no further edits.
- `tests/test_system_version_view.py` (10 new tests): find /
  find_one / count / aggregate read paths; non-matching filter
  returns empty; other-db `system.version` is empty; write
  rejection on insert / update / delete / drop with code 13.

### `renameCollection` cross-process safety — pinned by `WiredTiger.lock`

A backlog item ("renameCollection: atomic per the storage RLock,
but no protection against concurrent writers across worktrees")
turns out to be structurally addressed by WiredTiger itself.
`wiredtiger_open` takes an exclusive lock on the data directory at
open time; a second open on the same path fails with
``WT_ERROR Resource busy`` before any state is touched, so the
"concurrent writers across processes" scenario can't exist in the
first place.

Within-process atomicity is the storage `RLock`. Cross-process
exclusion is `WiredTiger.lock`. The two layers compose: rename is
safe under both. The backlog entry is struck through.

#### Added

- `tests/test_storage_exclusion.py` (2 new tests) pinning the
  guarantee: a second `Storage(path=...)` on the same on-disk
  directory raises a `WiredTigerError` whose message contains
  `"busy"`; the first instance keeps working unaffected.
  `rename_collection` survives a close + reopen round-trip — the
  renamed namespace is visible to a fresh `Storage` instance.

### `$setWindowFields` aggregation stage — minimum viable subset

The largest v1 stable-API stage that wasn't yet wired up.
`$setWindowFields` is mongod's windowed-analytics surface — running
totals, rolling averages, per-partition rankings — all expressed
as a partition + sort + per-row windowed accumulator over the
input. Driver test suites probe it heavily.

Spec shape::

    {
        partitionBy: <expression>,         # optional; default = single partition
        sortBy: <sort spec>,               # optional; default = input order
        output: {
            <field>: {
                <$accumulator>: <expr>,
                window: {documents: [<lower>, <upper>]},  # optional
            },
        },
    }

For each output field, the accumulator runs over the rows inside
that row's window — within the row's partition, in the partition's
sorted order. Original input order is preserved in the result; the
partition / sort dance is purely internal to compute the new
fields.

#### Shipped (first-cut subset)

* The nine `$group` accumulators: `$sum`, `$avg`, `$min`, `$max`,
  `$first`, `$last`, `$push`, `$addToSet`, `$count`. The dispatch
  reuses `_ACC_DISPATCH` from `$group` — same per-doc accumulator
  semantics, just applied over a per-row windowed subset.
* Position-based windows via `window: {documents: [<lower>, <upper>]}`.
  Bound forms: integer offsets relative to the current row,
  `"current"` (= 0), and `"unbounded"` (partition edge).
* Default window (omit `window`) covers the whole partition.
  `[unbounded, current]` gives running-total semantics;
  `[-1, 1]` gives a 3-doc rolling window; etc.
* Empty-window output values: 0 for `$sum`/`$count`, [] for
  `$push`/`$addToSet`, null for the rest (matches mongod).

#### Deferred (raise `AggregateError` with a clear message)

* Range-based windows (`window: {range: [...]}`, optionally with
  `unit:` for date ranges). Needs value-based bounds + date
  arithmetic; out of scope for the first cut.
* Time-series functions: `$derivative`, `$integral`, `$linearFill`,
  `$locf`, `$shift`, `$expMovingAvg`. Each is its own slice and
  not in the common driver-test surface.
* Rank functions: `$rank`, `$denseRank`, `$documentNumber`. These
  need sort-key equality detection (tied rows get the same rank).
  Worth a dedicated slice when a workload needs them.

#### Added

- `src/secantus/aggregate.py`: `_stage_set_window_fields` handler
  + helpers `_window_bounds` (resolves
  `documents: [<lower>, <upper>]` to inclusive partition indices,
  with clamping to partition edges) and `_empty_window_value`
  (mongod-matching defaults). Wired into `_STAGES`. Reuses
  `_ACC_DISPATCH` + `_finalize` from `$group` so the accumulator
  semantics stay aligned across the two stages.
- `tests/test_set_window_fields.py` (15 new tests): no-partition
  totals; partitionBy splits totals correctly; rolling 3-doc sum
  with edge clamping; `[unbounded, current]` running total;
  `[unbounded, unbounded]` per-partition total; `$avg` / `$min` /
  `$max` / `$first` / `$last` over `[-1, 1]`; `$count` over
  `[-1, 1]`; `$push` / `$addToSet` accumulating across rows;
  sortBy controls running-total order independently of input
  order; original input order preserved on output; rank function
  raises; range window raises; missing output rejected; multiple
  accumulators in one output rejected; empty input → empty out.

## [0.5.1b24] — 2026-05-19

### Geo: legacy `$near` sibling form, 2d quadtree covering, java gauge

Three geo improvements that close the long-standing tail of the
phase 1/2 geo work and lift the mongo-java-driver gauge into the
geo surface for the first time.

Legacy mongod 2d shape — `{geo: {$near: [x, y], $maxDistance: r,
$minDistance: r2}}` with the distance bounds at *sibling* level
rather than nested inside `$near` — now matches end-to-end through
both the operator matcher and the 2d-index picker. This is exactly
what `mongo-java-driver`'s `Filters.near(field, x, y, max, min)`
and `Filters.nearSphere(...)` build. Unit conventions match
mongod: legacy `$near` takes the bound in input units (planar
Pythagoras); legacy `$nearSphere` takes radians on the unit sphere
(picker converts to meters for 2dsphere and to degrees for 2d).

The 2d range scan picks tighter Z-order ranges via a quadtree
decomposition of the bbox: each 2^k × 2^k power-of-2-aligned
quadtree cell that lands fully inside the bbox emits one
contiguous Z-range (the invariant that makes Z-order indexes
work). Partial-overlap cells recurse; pure-outside cells are
skipped. Falls back to the single coarse range if the
decomposition would exceed `max_ranges=32`. Tightens the WT range
scan on wider query polygons; correctness is unchanged
(per-doc verifier filters false positives either way).

`mongo-java-driver`'s `GeoJsonFiltersFunctionalSpecification` and
`GeoFiltersFunctionalSpecification` (driver-core functional)
joined the java gauge include list and both pass 10/10. They
exercise `$geoWithin` / `$geoIntersects` / `$near` / `$nearSphere`
through the driver's `Filters` builder against a real 2d and
2dsphere index — the kind of integration coverage neither the
pymongo conformance gauge nor our in-tree pymongo tests reach.

#### Added

- `secantus.geo_index.planar_2d_covering_ranges()` — quadtree
  Z-order range decomposition for 2d index scans. Returns up to
  32 tight `(lo, hi)` ranges; falls back to a single coarse range
  on cap overflow.
- 6 new tests in `tests/test_geo_query.py` /
  `tests/test_geo.py`: sibling-form `$near` with `$maxDistance`,
  sibling-form annulus (max+min), sibling-form `$nearSphere`
  with radians convention, single-range quadtree for an aligned
  bbox, multi-range quadtree for an off-axis bbox, fallback to
  single range under cap.
- `_DRIVER_CORE_FUNCTIONAL_INCLUDES` in
  `java_validation/include_modules.py`: brings the two upstream
  geo functional specs into the java gauge as
  `:driver-core:test` filtered runs.
- [`docs/geospatial.md`](geospatial.md) — dedicated reference
  page: operator-by-operator, both index types, doc-side shapes
  accepted, the legacy / GeoJSON / spherical distance-unit
  conventions, a worked deployment example, validation surface
  summary. Linked from the Highlights list and added to the
  Sphinx toctree.
- [`docs/indexes.md`](indexes.md) — new geospatial section
  pointing at the dedicated page; the "Acceleration summary
  across index types" table now covers `2d`, `2dsphere`, and
  compound geo + scalar.

#### Changed

- `_parse_near_spec` now returns a 5-tuple
  `(center, max_d, min_d, spherical, legacy_form)`; consumers use
  the new `legacy_form` flag to pick the right unit conversion
  (legacy+spherical → radians; legacy+planar → input units;
  GeoJSON → meters).
- 2d-index picker uses the multi-range coverer; existing single-
  range `planar_2d_covering` kept as the coarse fallback.
- [`docs/indexes.md`](indexes.md) — "What's still missing" list
  rewritten. Multi-field sort acceleration, multikey indexing,
  and basic collation all shipped long ago and shouldn't have
  been on the gap list; the actual remaining gaps (per-index
  collation, TTL background sweeper, text / hashed indexes)
  replace the stale entries.
- [`docs/production.md`](production.md) — added a paragraph on
  per-write `writeConcern: {j: true}` routing as the
  finer-grained alternative to the daemon-wide
  `sync_on_commit = true` knob.

#### Fixed

- Legacy mongod `{geo: {$near: [x, y], $maxDistance: r}}`
  previously raised `unsupported query operator: $maxDistance`
  because the dispatcher treated the sibling bound as a
  standalone operator. The matcher now skips the sibling keys
  when iterating and passes them into `_op_geo_near`.
- 2d-index picker no longer over-filters on `$nearSphere` legacy
  form: the radians bound is converted to degrees before
  building the planar disk, matching mongod's behaviour against
  a 2d index.

## [0.5.1b23] — 2026-05-19

### Native TLS + mTLS + per-write `j:true` — production gaps closed

Three slices land together against the production-readiness gaps
called out in the `docs/production.md` page.

`[tls] cert_file` + `[tls] key_file` (in `secantusdb.toml`) or
`--tls-cert-file` / `--tls-key-file` (CLI) makes the daemon wrap
every accepted socket in TLS before the wire protocol starts.
Clients connect with `mongodb://host:port/?tls=true&tlsCAFile=<ca>`
and SecantusDB negotiates the TLS handshake itself; the
connection thread then sees an encrypted socket-like object and
serves mongo wire frames over it unchanged. This closes one of
the biggest production-deployment gaps the `docs/production.md`
page called out — operators no longer need to terminate TLS at an
nginx / HAProxy / stunnel reverse proxy that becomes part of the
trust boundary.

mTLS lands as a layer on top: set `[tls] ca_file` and the daemon
asks connecting clients for their own X.509 cert during the TLS
handshake, verifying it against the configured CA bundle. Set
`[tls] require_client_cert = true` to reject clients that don't
present a cert; the default (`false`, `CERT_OPTIONAL`) verifies a
cert if presented and accepts clients without one — useful for
staged rollouts. mTLS is a coarse-grained "you're someone we
approved of" gate; SCRAM-SHA-256 still identifies the specific
user on top. mongod's `MONGODB-X509` auth mechanism
(cert-subject-DN as the username, no SCRAM step) is a separate
follow-on slice.

Python's `PROTOCOL_TLS_SERVER` (TLS 1.2+, no SSLv2/3 fallback,
default cipher list) is the only protocol mode. The `SSLContext`
is built once at startup and cached — hot cert rotation requires
a daemon restart. `certbot renew --post-hook 'systemctl reload
secantusdb'` is the standard pattern. Without the cert / key
kwargs the daemon stays plaintext exactly as before — no
regression risk for the 1300+ existing tests.

The b20 `sync_on_commit` knob enabled per-commit fsync at the
*connection* level — every write on the daemon shared the same
durability mode. The third slice finishes the story: the per-write
`writeConcern.j` flag now threads from the wire layer through
`Storage.insert` / `update_matching` / `delete_matching` (and all
four `findAndModify` paths) into
`_batch_transaction(sync=True)`, which calls
`session.commit_transaction("sync=on")`. A client can now mix
`j: true` and `j: false` writes against one daemon: the j:true
subset pays the per-commit fsync cost (closes the durability gap),
the rest stays fast.

#### Added

- `[tls]` table in `secantusdb.toml` (`cert_file`, `key_file`,
  `ca_file`, `require_client_cert`). Half-configured TLS (only one
  of cert/key set) raises `ValueError` at startup so deployment
  mistakes can't silently fall back to plaintext.
- `--tls-cert-file` / `--tls-key-file` / `--tls-ca-file` /
  `--tls-require-client-cert` CLI flags. Standard precedence:
  SecantusConfig defaults < TOML < explicit CLI.
- `SecantusDBServer(tls_cert_file=..., tls_key_file=...,
  tls_ca_file=..., tls_require_client_cert=...)` kwargs. When
  cert/key are set an `ssl.SSLContext` is built in `__init__` and
  used to wrap accepted sockets in `_serve_forever`. When ca_file
  is also set, the context asks clients for an X.509 cert during
  the handshake and verifies it against that CA.
- `tests/test_tls.py`: 12 tests via `trustme` for ephemeral CA +
  client cert fixtures. Covers TLS round-trip, non-TLS-client
  rejection, no-args plaintext path (no regression),
  half-configured raises, missing-cert startup error,
  active_conns leak guard, and the four mTLS modes (required +
  valid cert / required + no cert / required + foreign-CA cert /
  optional + both modes).
- `journal: bool = False` kwarg on `Storage.insert` /
  `update_matching` / `delete_matching`. When True, the WT
  transaction commits with `session.commit_transaction("sync=on")`
  — forces a per-commit fsync of the log regardless of the
  connection's `transaction_sync` config.
- `_batch_transaction(*, sync: bool = False)` context-manager
  kwarg. The per-commit-fsync escape hatch the new `journal` write
  kwargs route through.
- `tests/test_write_concern_journal.py`: 10 tests covering the
  storage-layer kwarg threading (`_batch_transaction` is invoked
  with `sync=True/False` appropriately), wire-level happy paths
  on insert / update / delete / findAndModify, and the positive +
  negative routing assertions.

#### Changed

- TLS / mTLS handshake errors are logged + the socket closed +
  the active-connection slot released; the daemon keeps serving
  everyone else.
- `writeConcern: {j: true}` is now honoured per-write: the wire
  layer extracts the flag and threads it through to
  `_batch_transaction(sync=True)`. Previously the flag was
  accepted on the wire but had no effect — only the daemon-wide
  `sync_on_commit` knob (b20) could enable per-commit fsync.
- `docs/production.md` updated: "Native TLS" is no longer in the
  gaps list; the dedicated TLS section now shows the in-process
  config plus the mTLS opt-in instead of an nginx-stream-module
  example.
- `docs/configuration.md` documents the full `[tls]` schema
  (cert / key / ca / require_client_cert), the hot-rotation
  caveat, and the cipher-suite "out of scope for v1" note.

#### Dependencies

- `trustme>=1.2` added to the `dev` extra for the test CA
  fixture (transitively pulls `cryptography`).

## [0.5.1b20] — 2026-05-19

### `secantusdb.toml` config file, native checkpoint restore, j:true durability knob

Two production-shaping slices land together. A new
`secantusdb.toml` configuration file exposes every CLI flag plus
the WT and oplog knobs that were previously hard-coded — including
`cache_size` (so you can size the engine for your dataset instead
of running with the 1 GB test default) and a `sync_on_commit`
switch that closes the long-standing `writeConcern: {j: true}`
durability gap by enabling WT's per-commit fsync. The loader
auto-discovers `./secantusdb.toml`, `~/.secantus/secantusdb.toml`,
and `/etc/secantus/secantusdb.toml`; an explicit `--config PATH`
overrides the search. CLI flags still win over file values, so the
file is a deployment baseline rather than a lock-in.

A new `secantusAdmin.restoreArchive` wire command and matching
`secantusdb-restore-archive` offline CLI close out the backup
story started in b18 — extract a backup `.tar.gz` into a target
directory the operator then points a fresh SecantusDB process at.
The admin UI's per-row Restore button now adapts to backup type:
mongodump directories still call `mongorestore`; native `.tar.gz`
archives surface an inline target-dir field and an Extract action
that hits the new endpoint. Restore intentionally doesn't try to
swap the WT home under a running server (the connection-thread
session-caching layer would need a wholesale rework first), and
matches how real mongod restore tooling already trains operators.

Drive-by fix: the admin UI's "Existing backups" list now also
includes `.tar.gz` files. The native archives created by the b18
backup button were previously invisible because `list_backups`
only enumerated directories.

The new [Running in production](production.md) doc page ties the
config-file, native-backup, and restore work together — honest
comparison vs single-node Postgres (the more useful framing than
"SecantusDB vs mongod"), the gaps you have to accept, and a
concrete `systemd` / TLS / backup / monitoring deployment shape.

#### Added

- [Running in production](production.md) docs page — honest
  comparison vs single-node Postgres (the more useful framing than
  "SecantusDB vs mongod-for-prod"), the gaps you must accept (no
  native TLS, no PITR, no replication, beta maturity), and a
  concrete deployment shape: `systemd` unit, `secantusdb.toml`
  with `sync_on_commit = true`, SCRAM auth provisioning, nginx
  stream TLS termination, hourly native checkpoint backups with
  off-host sync, the restore drill, `serverStatus` scraping for
  Prometheus / Datadog, and capacity sizing notes for
  `cache_size`.
- `secantusdb.toml` configuration file (see
  [Configuration](configuration.md) for the full schema). Auto-
  discovered from `./secantusdb.toml`,
  `~/.secantus/secantusdb.toml`, `/etc/secantus/secantusdb.toml`;
  `--config PATH` disables discovery and loads a specific file.
  Unknown keys / unknown top-level tables fail loudly at startup
  so typos can't silently leave the engine running on the
  hard-coded default.
- `secantus.config.SecantusConfig` dataclass + `load_config()` /
  `apply_overrides()` helpers. CLI flags' argparse defaults are
  now `None` (the "user did not pass this" sentinel) so the
  precedence chain is `SecantusConfig defaults < secantusdb.toml
  < explicit CLI flag` — file is a per-deployment baseline, the
  CLI overrides for one-off runs.
- New CLI flags exposing previously-hard-coded knobs:
  `--cache-size`, `--session-max`, `--sync-on-commit`,
  `--oplog-retention-seconds`, `--oplog-max-entries`. Each has a
  matching `[storage]` / `[oplog]` key in the config file.
- `Storage.__init__` accepts `cache_size`, `session_max`,
  `sync_on_commit` kwargs. The WT engine config string is built
  from these instead of being a hard-coded literal.
- `secantusAdmin.restoreArchive` wire command. Accepts
  `archivePath` (server-side path to `.tar.gz`), `targetDir`
  (extraction destination), and optional `allowExisting` (overlay
  into a non-empty dir). Returns `{targetDir, fileCount, archive,
  ok: 1}`. RBAC: `fsync` action, cluster scope.
- `secantus.storage.extract_backup_archive(archive_path,
  target_dir, *, allow_existing=False)` — module-level helper
  shared by the wire command, the admin route, and the CLI.
  Validates that the archive contains a `WiredTiger` metadata
  file before unpacking, so a malformed tarball can't pollute the
  target.
- `secantusdb-restore-archive` console script (new `[project.scripts]`
  entry). Same validation as the wire command, no server needed.
- Admin UI per-row **Extract** action on `.tar.gz` rows, posting
  to `POST /backup/restore-archive` with editable target-dir form
  field; the existing `Restore` button still handles mongodump
  directories.

#### Changed

- `writeConcern: {j: true}` is now honourable end-to-end via
  `[storage] sync_on_commit = true` (or `--sync-on-commit`),
  which sets WT's `transaction_sync=(enabled=true,method=fsync)`.
  Closes the long-standing durability gap previously documented
  in the backlog. Off by default (matches mongod's default
  `{w:1, j:false}`) since the throughput cost is significant.
- `secantus.admin.backup.list_backups()` now includes
  `*.tar.gz` files alongside directories. Native-archive backups
  produced by b18's backup button were previously invisible in
  the admin UI's "Existing backups" list.
- `MongoFacade.restore_archive(archive_path, target_dir, *,
  allow_existing=False)` — new admin client facade method.

#### Fixed

- "Existing backups" table on `/backup` was silently dropping
  every `.tar.gz` produced by the native checkpoint backup path
  introduced in v0.5.1b18 (only dump *directories* were listed).
  Both kinds now render with the correct per-row restore action.

## [0.5.1b18] — 2026-05-18

### Native WT-checkpoint backups, admin UI /oplog page, and change-stream fidelity wins

The natural follow-on to v0.5.1b17's `local.oplog.rs` synthetic
collection lands as the admin UI `/oplog` page: a paged entry
browser with a window selector (last 50 / 500 / 5000), `op`-checkbox
filter (`i` / `u` / `d` / `c` / `n`), `ns` substring filter, and a
per-row expandable JSON body. Auto-refreshes every 5 s. The data
source is just `client.local.oplog_rs.find()` — no new server-side
surface needed, only the page chrome and an `_rows` partial that
follows the same pattern as `/connections` + `/cursors`.

`showExpandedEvents` on change streams now matches mongod: the flag
defaults to `false`, and DDL "expanded" events (`createIndexes`,
`dropIndexes`) are suppressed unless the user opts in via
`coll.watch(show_expanded_events=True)`. Previously these surfaced
unconditionally — more permissive than mongod, and broke the
conformance contract for tests that assume the stable v1 event set.

`killOp` lands as a real wire command that closes the target
connection's socket via `shutdown(SHUT_RDWR)`. Any in-flight command
finishes, the per-connection thread's next `recv` returns 0, the
loop exits, and the connection unregisters cleanly. Real mongod uses
a per-op interrupt flag, which would need cancellation infrastructure
SecantusDB doesn't carry — but "close the socket" is the visible
end-state users care about, and the kill-and-reap admin button on
`/connections` is now functional.

`$sample` becomes deterministic when `SECANTUS_SAMPLE_SEED=<n>` is
set in the environment. Builds a dedicated `random.Random(seed)`
instance at module load instead of mutating the global `random`
state, so other code sharing the process keeps its own entropy.
Closes the long-standing test-flake source where `$sample` results
varied run-to-run.

#### Added
- Admin UI `/oplog` page (`routers/oplog.py` +
  `templates/pages/oplog.html` + `templates/partials/oplog_rows.html`):
  window / op / ns filters, expandable per-row JSON, 5 s
  auto-refresh, sidebar entry between Profiler and Maintenance.
- `killOp` wire command + `kill(conn_id)` on
  `ConnectionRegistry` (shuts down the socket via
  `shutdown(SHUT_RDWR)`). Per-connection sockets are now stashed on
  the registry at `_handle_client` time.
- `A_KILLOP` privilege action in `secantus.rbac`; granted by
  `clusterAdmin` and `root`.
- Admin UI `/connections` Kill button (was a placeholder),
  typed-confirm modal (`partials/connection_kill_modal.html`),
  facade `kill_connection(conn_id)` method.
- `ChangeStreamSpec.show_expanded_events` parsed from
  `$changeStream.showExpandedEvents`; threaded into
  `changestreams.project`.
- `SECANTUS_SAMPLE_SEED` env var (read at `aggregate` module
  import) — `$sample` uses a dedicated `random.Random(seed)`
  when set.
- `secantusAdmin.backupArchive` wire command + `Storage.create_archive`
  + admin UI "Run native checkpoint backup" button: forces a WT
  checkpoint then tars the storage directory into a single
  `.tar.gz`. Faster + atomic vs `mongodump`; restore is "extract
  + start a new SecantusDB pointing at it". Rigorous round-trip
  test coverage in `tests/test_backup_restore.py` (doc identity at
  scale, every non-default index shape, oplog tail continuity,
  capped collection options + FIFO state, SCRAM users / roles,
  concurrent-writes consistency, archive portability, repeated-
  backup idempotency).
- `$densify` month / quarter / year units via
  `dateutil.relativedelta`. `quarter` is canonically 3 months.
  Adds `python-dateutil>=2.8` to the runtime dependencies (pure
  Python, available almost everywhere as a transitive dep).

#### Changed
- `changestreams.project` suppresses `createIndexes` / `dropIndexes`
  events unless the caller passed `show_expanded_events=True`
  (mongod-faithful default-off). The three existing tests +
  cross-driver DDL smokes (mongosh / node / go / java) all set the
  opt-in.

#### Fixed
- Closes backlog entry `$sample uses random.sample without a fixed
  seed` — deterministic via env var.
- Closes backlog entry `killOp / connection-close command` — admin
  UI Kill button is functional.
- Closes backlog entry `showExpandedEvents — accepted, ignored`.
- Closes backlog entry `Admin UI /oplog page`.
- `updateDescription.truncatedArrays` now emits for any array
  shrink (not just strict head-prefix), with indexed ``updatedFields``
  for kept-prefix changes — matches mongod's $v:2 in-place diff
  rather than wholesale-replacing on any reshape. Same-length-with-
  changes arrays also produce indexed ``arr.<i>`` updates now
  (previously wholesale). Closes the §3.2 backlog entry.

## [0.5.1b17] — 2026-05-17

### `local.oplog.rs` queryable from pymongo, `$merge` pipeline form + `$fill` stage + `$$var.path` resolution

Real mongod exposes the oplog as a queryable collection at
`local.oplog.rs` — pymongo clients can `db.oplog.rs.find()` against
it the same way they would against any collection. Until this release,
SecantusDB's oplog was internal only: `Storage.read_oplog` /
`oplog_floor_seq` / `oplog_tail_seq` were Python methods but had no
wire surface. Now `local.oplog.rs` is a synthetic read-only view —
`list_collections("local")` surfaces it, `find` / `count` /
`listCollections.options` route to a reader that walks the oplog WT
table directly, and write attempts (`insert`, `update`, `delete`,
`findAndModify`, `drop`, `create`, `createIndexes`) refuse with code
13 (Unauthorized) like mongod does. The deferred admin UI `/oplog`
page is unblocked as a follow-up; for now, debugging an in-flight
change-stream pipeline is as simple as
`client.local.oplog_rs.find({"op": "u"}).sort("ts", -1).limit(20)`.

The aggregation expression library picks up two of the three remaining
stages on most "more stages" wishlists. `$merge` was partly
implemented; this batch fills in the rest: `whenMatched: [<pipeline>]`
runs a sub-pipeline against the matched target doc with `$$new` bound
to the source doc and any user `let` vars threaded through;
`whenMatched: "delete"` (MongoDB 5.0+) removes the matched doc; a
unique-index guard refuses non-`_id` `on` fields without a `unique:
true` index covering them, matching mongod's rule against silent
on-field collapse.

`$fill` lands fresh — the 5.3+ stage for filling missing/null fields.
Three modes per output field: `{value: <expr>}` replaces with an
evaluated expression; `{method: "locf"}` carries the last observation
forward within the partition's sortBy order; `{method: "linear"}`
interpolates between bracketing non-null anchors along the sortBy field
(works for numbers and datetimes — timedelta arithmetic divides cleanly
to float and multiplies back to timedelta). Partitioning via
`partitionByFields` or `partitionBy`; sortBy required when any output
uses `method`.

The `$merge` pipeline form was the first thing in the repo to exercise
`$$var.path` (e.g. `$$new.delta`), and surfaced that the expression
evaluator only did exact-name var lookup. Fixed in the same batch:
`$$var.field.path` now walks the dotted path into the resolved value
across `$$ROOT.f` / `$$CURRENT.f` / user-let vars.

#### Added
- `local.oplog.rs` synthetic collection: queryable via `find` /
  `count` / `listCollections`. Walks the existing oplog WT table via
  a private session for cross-thread visibility. `list_databases`
  surfaces `local` whenever the oplog is enabled.
- `$merge whenMatched: [<pipeline>]` with `$$new` binding + `let` clause
  for user-defined vars (`aggregate._stage_merge`).
- `$merge whenMatched: "delete"` (MongoDB 5.0+).
- `$merge` unique-index guard on non-`_id` `on` fields.
- `$fill` stage with `value`, `locf`, and `linear` modes
  (`aggregate._stage_fill`).
- `$$var.field.path` dotted-path resolution in
  `expressions._resolve_var`.
- `docs/changelog.md` as the system of record (see the
  [changelog](changelog) itself and the `changelog/` Python package
  that generates blog posts from it).

#### Changed
- Writes to `local.oplog.rs` (insert / update / delete / findAndModify
  / drop / create / createIndexes) refuse with code 13 (Unauthorized).
- `$merge` validates `whenMatched` / `whenNotMatched` against the
  allowed string sets — typos surface as `AggregateError` instead of
  silently falling through to the default merge.

## [0.5.1b16] — 2026-05-16

### Sidebar grouping, auto-refreshing connections and cursors, Roles in the nav

The `/connections` and `/cursors` admin pages have always been live-data
views — they read `currentOp` and render the connection / cursor list
each time the page is requested — but they didn't refresh. The dashboard
polls 1 Hz over a WebSocket; these two felt stale next to it. v0.5.1b16
extracts each table's tbody into an HTMX partial and lets the tbody
itself swap every 5 s via `hx-trigger="every 5s"
hx-get="/connections/_rows"`. The page chrome and column headers stay
fixed; only the rows refresh. `/connections` also gains a (disabled)
Actions column with a tooltip explaining that connection-kill is
deferred until SecantusDB grows `killOp` — purely a layout-symmetry
fix so the page mirrors the shape of `/cursors`.

The sidebar gets two structural fixes. A `Roles` entry now lives
directly under `Users` with a sub-nav indent (it was reachable only via
the breadcrumb on `/users`, and `roles.html` was setting `active="users"`
so the wrong sidebar item highlighted while you were on the page). A
second visual separator above `Change stream` marks the boundary between
per-target data pages and operational-state pages, mirroring the
existing separator below `Server`.

A separate fix: 15 `*_via_mongosh` cross-driver smoke tests are now
grouped into a single xdist worker. Mongosh launches a full Node-based
shell, and under heavy parallel load the PBKDF2 work inside SCRAM-SHA-256
auth could blow past mongosh's connect timeout. Tagged with
`@pytest.mark.xdist_group(name="mongosh_smokes")` so they serialize.

#### Added
- Sidebar `Roles` entry under `Users` with sub-nav indent + correct
  active highlight on `/roles`.
- Sidebar visual separator above the operational-state group
  (`nav-ops-start` CSS class).
- `/connections` + `/cursors` auto-refresh tbody (`hx-trigger="every 5s"`)
  with new `_rows` partial endpoints.
- `/connections` disabled Actions column for layout symmetry with
  `/cursors`; tooltip explains `killOp` is deferred.

#### Fixed
- 15 `*_via_mongosh` cross-driver smoke tests serialized via
  `xdist_group="mongosh_smokes"` to dodge PBKDF2-handshake timeouts under
  parallel-test CPU contention.

## [0.5.1b15] — 2026-05-16

### One scaffold for every confirmation modal — escape, focus-trap, restored focus

The `secantus-admin` UI has nine confirmation / edit modals
(drop-database, drop-collection, drop-index, drop-user, change-password,
manage-roles, edit-document, delete-document, kill-cursor). They were
assembled at slightly different times and drifted in five different ways
— different destructive-button copy, different typed-confirm targets
(the delete-document modal asked the user to type the collection name
shared by every row; the kill-cursor modal asked for the giant int
cursor id), no Escape-to-close, no focus restoration to the trigger
element, no focus trap so Tab leaked back into the page behind, and
`aria-label="Close"` only on two of nine close buttons.

v0.5.1b15 consolidates all nine on a shared scaffold: a new
`modal-shell.js` exposes `openModal(url)` / `closeModal()` /
`setupModal(el)` plus a global htmx hook that captures the trigger
element so `closeModal()` can restore focus. Each modal partial has the
same overlay shape — `x-init="setupModal($el)"`,
`@click.self="closeModal()"`, `@keydown.escape.window="closeModal()"`,
`role="dialog"`, `aria-modal`, `aria-labelledby` — and Tab / Shift+Tab
cycle within the modal's focusable children rather than escaping into
the page behind.

Three substantive fixes ride along with the scaffolding: destructive
button copy now always restates action+noun (Kill cursor / Delete
document / Drop index / Drop user / Drop database / Drop collection);
the delete-document typed-confirm asks for the doc's `_id` value rather
than the collection name; the kill-cursor typed-confirm asks for the
collection `ns` rather than the unguessable cursor id. None of these
change SecantusDB's wire-protocol behaviour.

#### Added
- `static/js/modal-shell.js`: `openModal(url)`, `closeModal()`,
  `setupModal(el)`, htmx hook for trigger-element capture.
- `[x-cloak]` CSS helper to prevent Alpine flash on first paint.

#### Changed
- All 9 confirmation / edit modal partials use the shared overlay
  shape with `role="dialog"` / `aria-modal` / `aria-labelledby`.
- Destructive button copy restates action+noun across the board.
- `delete-document` typed-confirm uses the doc's `_id` value (was the
  collection name).
- `kill-cursor` typed-confirm uses the collection `ns` (was the cursor
  id).

#### Fixed
- Escape now closes every modal.
- Focus restored to the triggering element after modal close.
- Tab focus-trap inside modals.
- `aria-label="Close"` on all 9 close buttons (was on 2).

## [0.5.1b14] — 2026-05-15

### Admin UI punch list — five silent-failure modes fixed

The May 2026 end-to-end review of the `secantus-admin` web UI catalogued
five P0s — bugs that didn't crash anything but presented wrong
information to the user. v0.5.1b14 fixes all five. None require any
database-level change; this is purely admin-UI plumbing, but each one
was either lying to the user or hiding a real error behind cheerful
copy.

The biggest was the **profiler page swallowing every exception** while
reading `system.profile`. A bare `except Exception:` rendered "no
entries yet — run an operation to see one appear here" no matter what
the underlying error was, including the target server being completely
unreachable. The clause is now narrowed to `PyMongoError` and the
friendly error message gets funnelled into the page's normal error
banner. The same page also had a **`flash` keyword argument that the
template never rendered** — every settings change returned `HX-Redirect`
and the user saw zero confirmation that anything had happened. The POST
handler now re-renders the page inline with a flash banner that names
the new level / slowms / sampleRate values.

The other three are dead-code cleanups: the **doc tour** in
`docs/admin.md` walked the user through a `/console` page that was
renamed to `/query` two refactors ago; the **Maintenance "Drop
collection" form** had an `hx-get` pointing at a route that never
existed; and the **dashboard router** still exposed a `GET
/_partials/dashboard-tiles` endpoint from before the WebSocket dashboard
landed.

#### Fixed
- Profiler page: narrowed bare `except Exception:` to `PyMongoError` so
  server-down errors surface (`routers/profiler.py`).
- Profiler page: added flash banner block to template + POST handler
  re-renders inline instead of `HX-Redirect`.
- Maintenance "Drop collection" form: dropped dead
  `hx-get="/maintenance/drop-collection-redirect"` attribute.
- Dashboard router: deleted unused
  `GET /_partials/dashboard-tiles` endpoint, partial template, and the
  two tests that exercised them.
- `docs/admin.md`: replaced stale `### Console` section with
  `### Query (/query)` + `### Insert (/insert)` + new `### Server
  (/server)` subsection.

## [0.5.1b13] — 2026-05-15

### Zero actionable failures — every driver gauge classified, every gap explained

Over the past few releases the cross-driver gauge pass rate has been
climbing — 99.5% at v0.5.1b4, 99.9% by last week's refresh. The last
0.1% was a handful of failures that either could not be fixed in
SecantusDB (a Java-driver SDAM cascade triggered by a server-side
`APIStrictError`), reproduced only under heavy parallel load (two
`mongo-go-driver` flakes), or assumed a multi-node replica-set
deployment SecantusDB deliberately doesn't simulate (Ruby's `w: 2`
write-concern test). Reporting them as plain "failures" overstated the
gap — but silently dropping them would let real regressions hide in the
same column.

v0.5.1b13 introduces **`validation_summary/expected_failures.py`** — a
small per-gauge registry of `(pattern, rationale)` entries. The
cross-driver summary now separates "Failed" (unexpected, a real bug we
need to fix) from "Expected" (a documented gap with a one-line reason
that ships in the report). A new **Adjusted** column reports the rate
excluding expected failures from the denominator — "how much of the
conformable surface actually conforms." Current numbers: **7,186 tests,
6,254 passed, 0 unexpected failures, 5 expected failures, 927 skipped —
100.0% adjusted across every driver.**

This release also bundles the gauge improvements that landed since
v0.5.1b4: `mapReduce` returns a graceful empty result for non-canonical
bodies, `$changeStream` against a standalone topology is rejected with
code `40573`, Node CSOT explain-plus-`timeoutMS` tests pass via a new
`block_connection` / `block_time_ms` failpoint pair, `getParameter`
advertises `authenticationMechanisms: ["SCRAM-SHA-256"]`, and
`createIndexes` / `create` reject unknown options up-front.

#### Added
- `validation_summary/expected_failures.py`: per-gauge registry of
  documented-known failures with rationales.
- Cross-driver summary "Expected" + "Adjusted pass rate" columns.
- `block_connection` / `block_time_ms` failpoint fields
  (`failpoints._FailCommand`).

#### Changed
- `mapReduce` returns a graceful empty result for non-canonical
  map/reduce bodies (wire-shape probes pass).
- `$changeStream` on a standalone topology is rejected with code 40573.
- `getParameter` advertises `authenticationMechanisms: ["SCRAM-SHA-256"]`.
- `createIndexes` rejects unknown per-index options
  (`_INDEX_SPEC_KNOWN_OPTIONS` whitelist).
- `create` rejects unknown collection options
  (`_CREATE_KNOWN_OPTIONS` whitelist).
- `validate-all` serialized (`max_workers=1`) to dodge load-induced
  inter-gauge flakes.

## [0.5.1b4] — 2026-05-12

### Cross-driver conformance summary — 99.5% across 7,186 tests on one page

Until this release, comparing SecantusDB's conformance across the five
driver gauges (pymongo / mongo-java-driver / mongo-go-driver /
mongo-node-driver / mongo-ruby-driver) required opening five different
reports and squinting at five different per-category breakdowns whose
denominators came from incompatible units of count — JUnit `<testcase>`
versus Mocha test versus RSpec example versus `go test` event versus
pytest item.

v0.5.1b4 ships **`docs/validation-summary.md`** — a single table that
normalises on test count, one row per gauge, the same five columns
across the board: tests run, passed, failed, skipped, pass rate. A new
`validation_summary` Python module reads each gauge's raw artifact
under `.validation/` directly and renders the table; a new
`invoke validate-summary` task refreshes it.

Current numbers: **7,186 tests, 6,232 passed, 33 failed, 921 skipped —
99.5% pass rate** across all five drivers. Java is biggest by raw count
(4,710 tests, 4,242 passed); Node smallest (364).

This release also rolls up two driver-gauge fixes that landed since
v0.5.1b1: a Java widening to 21 of 112 driver-sync functional classes
(+34 passes), and a snapshot-read-concern rejection that turned three
`SessionsTest` snapshot-error scenarios from "expected error, got
success" into "expected error, got `SnapshotUnavailable` (code 246)".

#### Added
- `docs/validation-summary.md` cross-driver normalized table.
- `validation_summary/` Python module (raw-artifact reader + renderer).
- `invoke validate-summary` task.
- `snapshot` readConcern rejected with code 246
  (`SnapshotUnavailable`).
- Java gauge: `ChangeStreamsTest`, `UnifiedWriteConcernTest`,
  `VersionedApiTest` unified-spec runners (21 of 112 driver-sync
  functional classes total).

#### Fixed
- RTD build for v0.5.1b3 failed on a missing toctree entry for the new
  summary file; b4 is the first release where the docs match what's on
  PyPI.

## [0.5.1b1] — 2026-05-12

### Java gauge scope made honest — 18 of 112 driver-sync classes, five named follow-ups

The Java gauge passing rate had been reported at "100%" — but only
across the 13 driver-sync functional classes the gauge was running.
v0.5.1b1 widens the include set to 18 of 112 and adds an explicit
**Scope** section to `docs/validation-report-java.md` that surfaces the
"X of 112 driver-sync functional classes" denominator so the headline
number isn't misleading.

The widened set surfaced five real failures, all named and tracked in
`tasks/backlog.md` §5: Java apiStrict pool-clear cascade, mapReduce
non-canonical bodies, snapshot reads on standalone, distinct
apiStrict — none are SecantusDB bugs, but they're now documented
expected-fail entries.

#### Added
- Java gauge include set widened to 18 of 112 driver-sync functional
  classes (`java_validation/include_modules.py` waves 1 + 2).
- "Scope" section in Java validation report exposing the include-set
  denominator (`java_validation/generate_report.py`).

## [0.5.0b18] — 2026-05-12

### Ruby gauge climbs to 99%, completing the cross-driver 99–100% band

The Ruby gauge had been the weakest of the five at ~95% — a handful of
real SecantusDB gaps the Ruby driver exercises but the others don't.
v0.5.0b18 closes the high-value ones: `writeConcernError` is now
attached on `w > 1` (CannotSatisfyWriteConcern code 100), invalid
`wildcardProjection` is rejected on `createIndexes`, `commitQuorum` is
validated at the top level, `listIndexes` rejects negative batchSize
(code 51024), and `$collStats` surfaces capped-collection bounds
(`storageStats.{capped, max, maxSize}`).

Net: Ruby gauge from 94.6% → 99.7%, 13 net passes. All five driver
gauges now sit in the 99–100% band.

#### Added
- `writeConcernError` attached on `w > 1` (`CannotSatisfyWriteConcern`
  code 100).
- `createIndexes` validates `wildcardProjection` shape.
- `commitQuorum` validated at top-level.
- `$collStats` surfaces capped bounds (`storageStats.{capped, max,
  maxSize}`).

#### Changed
- `listIndexes` rejects negative `batchSize` with code 51024.

## Older releases

Releases before v0.5.0b18 (the `v0.3.0aN` and `v0.4.0bN` lines, and
v0.5.0b1 through v0.5.0b3) shipped before this changelog was the system
of record. See the [GitHub
Releases](https://github.com/jdrumgoole/SecantusDB/releases) page for
the auto-generated commit-list notes from those tags.

[Unreleased]: https://github.com/jdrumgoole/SecantusDB/compare/v0.5.1b18...HEAD
[0.5.1b24]: https://github.com/jdrumgoole/SecantusDB/releases/tag/v0.5.1b24
[0.5.1b23]: https://github.com/jdrumgoole/SecantusDB/releases/tag/v0.5.1b23
[0.5.1b20]: https://github.com/jdrumgoole/SecantusDB/releases/tag/v0.5.1b20
[0.5.1b18]: https://github.com/jdrumgoole/SecantusDB/releases/tag/v0.5.1b18
[0.5.1b17]: https://github.com/jdrumgoole/SecantusDB/releases/tag/v0.5.1b17
[0.5.1b16]: https://github.com/jdrumgoole/SecantusDB/releases/tag/v0.5.1b16
[0.5.1b15]: https://github.com/jdrumgoole/SecantusDB/releases/tag/v0.5.1b15
[0.5.1b14]: https://github.com/jdrumgoole/SecantusDB/releases/tag/v0.5.1b14
[0.5.1b13]: https://github.com/jdrumgoole/SecantusDB/releases/tag/v0.5.1b13
[0.5.1b4]: https://github.com/jdrumgoole/SecantusDB/releases/tag/v0.5.1b4
[0.5.1b1]: https://github.com/jdrumgoole/SecantusDB/releases/tag/v0.5.1b1
[0.5.0b18]: https://github.com/jdrumgoole/SecantusDB/releases/tag/v0.5.0b18
