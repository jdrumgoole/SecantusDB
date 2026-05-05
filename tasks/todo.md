# Admin UI (pywebview) — implementation plan v2

Branch: `admin-ui` · Worktree: `../SecantusDB-admin`

## Locked decisions

- **Frontend**: HTMX + Alpine.js + Chart.js (+ Leaflet on the geo page). No React.
- **Connection**: pymongo over the wire only; `--uri` flag. No `--embed` mode in v1.
- **Profiler**: real MongoDB-compatible `profile` command (`{profile, slowms}`), persisted to a per-DB capped `system.profile` collection (10 MB cap). UI just queries it.
- **Admin UI auth**: none; bind loopback + URL token.
- **Packaging**: `secantusdb[admin]` extra; `secantusdb-admin` console script.
- **Pagination v1**: skip-ID only (sort by `_id` asc/desc; cursor = base64(last `_id`)). Custom-sort + paginate errors with a clear message until a later slice adds real-pymongo-cursor pagination.
- **Tests**: parallel-safe with `port=0` + `tmp_path`. `httpx.AsyncClient(transport=ASGITransport(app))` + in-process SecantusDB.
- **Wire shape**: implement mongod-shape commands (`currentOp`, `fsync`, real `getLog` + `buildInfo`) instead of `secantusAdmin.*`. Honest dogfooding; commands are reusable beyond the admin UI.
- **`fsync`**: accept; error if `lock: true`.
- **Token**: fixed (persisted to `~/.secantus/admin-token` on first launch; `--token <override>` to override).
- **Theme**: lift website palette tokens (`--bg`, `--fg`, `--accent`) and font stack into `admin.css`. Layout is admin-tool-shaped (left sidebar, dense grids).
- **Slice 1 → push + bump**: when Slice 1 lands, bump version, commit, push, merge.

## Slice 0 — Capped collections — DONE (merged into main)

## Slice 1 — Skeleton + small server-side hooks (combined)

Sub-slices for shippable green builds:

### 1.0 — pyproject + extras
- [ ] `[project.optional-dependencies].admin` = fastapi, uvicorn[standard], jinja2, httpx, pywebview, python-multipart
- [ ] `[project.scripts].secantusdb-admin = "secantus.admin.cli:main"`
- [ ] `uv lock` (will refresh)

### 1.1 — Server primitives
- [ ] `src/secantus/connreg.py`: `ConnectionRegistry` + `ConnInfo` (open / close / record_command / authenticate / snapshot)
- [ ] `src/secantus/logbuf.py`: `LogBuffer` ring buffer (capacity 5000)
- [ ] `Storage.checkpoint()`: force WT checkpoint
- [ ] `CursorRegistry.snapshot()`: list active cursors with `id, ns, batch_remaining, opened_at, last_access`
- [ ] `SecantusDBServer` wires `ConnectionRegistry` + `LogBuffer` into accept loop and per-conn finally
- [ ] Tests (storage-level)

### 1.2 — Real commands using the primitives
- [ ] `currentOp` command: returns `{inprog: [...], ok: 1}` mongod-shape, drawn from connreg + cursorreg
- [ ] `fsync` command: accept; error 9 (`Location9: lock: true is not supported`) if `lock: true`; else call `Storage.checkpoint()` and return `{numFiles: 1, ok: 1}`
- [ ] `getLog` command: real backing — read from `LogBuffer.tail(n)`; replace stub
- [ ] `buildInfo` command: read `secantus.__version__`; replace hardcoded "7.0.0" only on the SecantusDB-version field, retain wire-protocol identity fields the drivers expect
- [ ] Tests: pymongo-driven in `tests/test_crud.py` (currentOp, fsync) and new `tests/test_admin_commands.py` (getLog, buildInfo)
- [ ] Update `tasks/backlog.md`: strike through `getLog`, `buildInfo` stubs

### 1.3 — Admin package skeleton
- [ ] `src/secantus/admin/{__init__.py,__main__.py,cli.py,app.py,launcher.py,client.py,middleware.py}`
- [ ] argparse: `--uri`, `--port`, `--no-window`, `--token`, `--token-path`
- [ ] `create_app(*, mongo_uri, token)` returns FastAPI; mounts `/healthz`, `/`, `/static`
- [ ] `MongoFacade` thin pymongo wrapper (timeouts, server selection, lazy)

### 1.4 — Token + dashboard
- [ ] Middleware: token check on all non-`/healthz`, non-`/static` routes; query-string `?t=<token>` OR `X-Admin-Token` header OR cookie set on first auth
- [ ] First-load handler sets `secantus-admin-token` cookie (HttpOnly, SameSite=Strict)
- [ ] Token resolution: `--token` flag → `~/.secantus/admin-token` file → generate + persist new
- [ ] `/healthz` returns `{ok: true, version, mongo_ok}` (mongo_ok = quick `ping` against target)
- [ ] `/` dashboard: KPI tiles (uptime, conns current/total, opcounters, network requests). Polls `serverStatus` every 2s via HTMX `hx-trigger="every 2s"` against `/_partials/dashboard-tiles`. WebSocket comes in Slice 4.
- [ ] Templates: `base.html`, `dashboard.html`, `partials/kpi.html`
- [ ] `static/css/admin.css`: lift website palette tokens; sidebar + tile styles
- [ ] `static/js/{htmx.min.js,alpine.min.js}`: vendored at fixed versions

### 1.5 — Launcher + smoke tests
- [ ] `launcher.py`: uvicorn-in-thread, wait for `/healthz`, open pywebview window, clean shutdown on close
- [ ] `tests/test_admin_skeleton.py`: healthz, token enforcement, dashboard renders, MongoFacade reads serverStatus
- [ ] All tests pass parallel via `uv run python -m invoke test`

### 1.6 — Invoke task
- [ ] `tasks.py`: `invoke admin --uri ... --port 0` task

### 1.7 — Push + bump
- [ ] Bump `pyproject.toml` + `src/secantus/__init__.py` to next aN
- [ ] `uv lock`
- [ ] Commit with co-author trailer
- [ ] Push admin-ui to origin
- [ ] Merge admin-ui into main with `--no-ff`
- [ ] Push main

## Slice 2 — Databases & collections + paginated collection viewer

Pure UI on existing wire surface.

- [ ] `MongoFacade` paged-list helpers (`paged_collection`, `parse_cursor`, `next_cursor`)
- [ ] `/db` tree (databases → collections); per-coll stats from `dbStats`/`collStats`
- [ ] `/db/{db}/{coll}` viewer: filter (JSON parsed via `bson.json_util.loads`), `_id` sort dropdown, paged list
- [ ] Edit-row modal (whole-doc JSON, _id immutable, `replace_one`)
- [ ] Delete-row typed-confirmation modal
- [ ] Tests

## Slice 3 — Indexes & explain visualizer

- [ ] `/db/{db}/{coll}/indexes`: list with multikey/partial/TTL/2dsphere/direction badges
- [ ] Create / drop index forms
- [ ] `/explain`: filter+sort+hint inputs → render winningPlan tree (FETCH→IXSCAN/COLLSCAN; multi-field sort acceleration shown as IXSCAN with directions)
- [ ] Tests

## Slice 4 — Real-time metrics dashboard

`serverStatus` is real now — pure UI/WS slice.

- [ ] `metrics.py` (admin-side): 1 Hz sampler thread polling pymongo `serverStatus`; bounded 300s deque; computes deltas
- [ ] `/ws/metrics` WebSocket; client reconnects with backoff
- [ ] Replace 2s polling on dashboard with WS push; Chart.js sparklines
- [ ] Tests: WS round-trip (starlette TestClient)

## Slice 5 — Users + roles

RBAC is real. Surface it fully.

- [ ] `/users`: list (`usersInfo`), create (`createUser`, role picker from `rolesInfo`), change password (`updateUser` with `pwd`), drop (`dropUser`)
- [ ] Grant / revoke roles (`grantRolesToUser` / `revokeRolesFromUser`)
- [ ] `/roles`: read-only list of built-in roles + their actions
- [ ] Tests

## Slice 6 — Oplog + change-stream tail

- [ ] `/oplog`: window inspector (oldest/latest ts, retention seconds, entry count); load older / live tail
- [ ] `/changestream/{scope}` WS: tail `$changeStream` at coll/db/cluster scope
- [ ] DDL filter toggle (createIndexes/dropIndexes events flow now)
- [ ] Resume-token copy
- [ ] Tests

## Slice 7 — Ad-hoc query console

- [ ] `/console`: tabs find / aggregate / runCommand
- [ ] Recent queries persisted in `~/.secantus/admin.db` (sqlite, per-URI)
- [ ] Result viewer reuses Slice 2's pagination
- [ ] Tests

## Slice 8 — Connections + cursors page (UI only)

Server side already done in Slice 1.

- [ ] `/connections` table: list ConnInfo from `currentOp`; close-connection action (closes the TCP socket; re-confirms `killOp` deferred)
- [ ] `/cursors` table: list cursors from `currentOp`; kill-cursor action via `killCursors`
- [ ] Tests

## Slice 9 — Profiler

Capped is done; system.profile lands cleanly.

- [ ] `profile` command: `{profile: 0|1|2, slowms: N, sampleRate?}`; persist per-DB in `admin.system.profileSettings`-style doc
- [ ] Dispatch path times each op (`time.monotonic_ns`); on level-2 or `dur >= slowms`, write entry to `<db>.system.profile` (auto-created capped, 10 MB)
- [ ] Profile entry shape mongod-faithful: `op`, `ns`, `command`, `ts`, `millis`, `planSummary`, `keysExamined`, `docsExamined`, `nreturned`, `client`, `user`, `appName`
- [ ] `/profiler`: settings form + paged list filtered by ns / op / min-duration
- [ ] Tests

## Slice 10 — Maintenance

- [ ] `/maintenance`: prune_oplog, prune_ttl, fsync (checkpoint), drop coll/db buttons
- [ ] Typed-confirmation modal ("type the collection name to confirm")
- [ ] Tests

## Slice 11 — Schema sampler, geo viewer, logs viewer, settings

- [ ] Schema sampler: sample N docs, infer field paths/types/cardinality/null-rate
- [ ] Geo viewer: Leaflet map for collections with a 2dsphere index; render first N matches
- [ ] Logs viewer: queries `getLog` (now real); tail-stream via WS
- [ ] Settings: saved connections in sqlite, refresh intervals, dark/light synced with OS
- [ ] Tests

## Slice 12 — Backup / restore

- [ ] `mongodump`/`mongorestore` subprocess path (preflight checks tools on PATH)
- [ ] Native "WT checkpoint → tar" path using `Storage.checkpoint()` from Slice 1
- [ ] UI form + progress stream
- [ ] Tests

## Documentation

- [ ] `docs/admin.md`: install (`pip install 'secantusdb[admin]'`), launch, screenshots, security model, slice list
- [ ] README "Admin UI" section + screenshot
- [ ] Update `tasks/backlog.md` for anything still stubbed at end of each slice

## Cross-branch awareness

- The `rbac` branch is merged. Auth gaps (`updateUser`, RBAC enforcement) are closed. No coordination needed.
- Don't touch other worktrees, branches, tags, or stashes.

## Lessons / surprises

(Capture here as we go; promote to `tasks/lessons.md` if generally applicable.)

- Slice 0: original "reject deletes/shrinking-updates on capped" framing was based on pre-5.0 mongod. Real 7.0 allows them, so the slice landed as "match 7.0" + eviction-on-update. Rule: when a slice plan cites old mongod semantics, double-check current behaviour before locking the contract.

## Review

(Fill in after each slice merges to main.)

- Slice 0: capped collections + FIFO eviction. 10 new tests, full suite 741 green. Merged in `abb00b9`.
