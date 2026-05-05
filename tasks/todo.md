# Admin UI (pywebview) — implementation plan

Branch: `admin-ui` · Worktree: `../SecantusDB-admin`

## Locked decisions

- **Frontend**: HTMX + Alpine.js + Chart.js (+ Leaflet on the geo page). No React.
- **Connection**: pymongo over the wire only; `--uri` flag. No `--embed` mode in v1.
- **Profiler**: real MongoDB-compatible `profile` command (`{profile, slowms}`), persisted to a per-DB capped `system.profile` collection. UI just queries it.
- **Admin UI auth**: none; bind loopback + per-launch URL token.
- **Packaging**: `secantusdb[admin]` extra; `secantusdb-admin` console script. Base wheel unaffected.
- **Pagination**: cursor-based on every list endpoint. No `to_list()`.
- **Tests**: parallel-safe with `port=0` + `tmp_path`. `httpx.AsyncClient` for unit; Playwright for end-to-end.

## Cross-branch awareness

- Parallel worktree `../SecantusDB-rbac` on branch `rbac` is touching auth/roles. Keep the admin UI Users page thin and table-driven so it absorbs whatever lands there. Re-verify `auth.py` shape before wiring the Users router.
- Do not touch other worktrees, branches, tags, or stashes.

## Slices

### Slice 0 — Capped collections (prerequisite for the profiler)

- [x] `create` command accepts `capped: true`, `size: <bytes>`, `max: <docs>`; persist options blob
- [x] `Storage.insert` enforces FIFO eviction when capped bounds exceeded
- [x] `listCollections` surfaces capped flags
- [ ] **Revised from "reject unsupported ops"**: mongod 7.0 allows deletes and size-changing updates on capped collections. Match that. Add eviction-on-update so size-growing updates still honor cap. Tests cover both.
- [ ] Unit + pymongo integration tests
- [ ] Update `tasks/backlog.md` if anything is left stubbed

### Slice 1 — Skeleton

- [ ] `admin` extra in `pyproject.toml`: `pywebview`, `fastapi`, `uvicorn[standard]`, `jinja2`, `httpx`, `python-multipart`
- [ ] `secantusdb-admin` console script wired to `secantus.admin.cli:main`
- [ ] `src/secantus/admin/{__init__.py,__main__.py,cli.py,launcher.py,app.py,client.py}`
- [ ] argparse CLI: `--uri`, `--port` (default 0), `--no-window` (headless for tests)
- [ ] FastAPI app factory, loopback bind, per-launch random token middleware
- [ ] pywebview window opener + uvicorn-in-thread launcher
- [ ] Empty dashboard page renders
- [ ] `invoke admin` task to launch against a local server
- [ ] Smoke test: launch + curl `/healthz` returns 200

### Slice 2 — Databases & collections + paginated collection viewer

- [ ] `MongoFacade` wrapping pymongo (timeouts, retries, pagination helpers)
- [ ] `/db` tree (databases → collections); per-coll stats from `dbStats`/`collStats`
- [ ] `/db/{db}/{coll}` collection viewer: cursor-based paged docs, filter/projection/sort form
- [ ] JSON tree view + raw view toggle (Alpine)
- [ ] Edit / delete row (with typed-confirmation modal)
- [ ] Tests: `httpx.AsyncClient` against in-process SecantusDB

### Slice 3 — Indexes & explain visualizer

- [ ] `/db/{db}/{coll}/indexes`: list with multikey/partial/TTL/2dsphere/direction flags
- [ ] Create / drop index forms
- [ ] `/explain` form: filter + sort + hint inputs → `_explain` → render `winningPlan` (FETCH > IXSCAN/COLLSCAN, indexName, keyPattern, direction)
- [ ] Tests

### Slice 4 — Real-time metrics dashboard

- [ ] `metrics.py`: serverStatus delta sampler thread (1 Hz), bounded 300s deque
- [ ] `/ws/metrics` WebSocket: broadcast ticks; client reconnects with backoff
- [ ] Dashboard KPI tiles + Chart.js sparklines (ops/sec by op type, conns, data size, oplog window)
- [ ] Tests: WS round-trip with `httpx_ws` or starlette TestClient

### Slice 5 — Users

- [ ] `/users`: list, create (SCRAM-SHA-256 default), change password, drop
- [ ] Roles surface as a read-only column for now; revisit after `rbac` branch lands
- [ ] Tests

### Slice 6 — Oplog & change-stream tail

- [ ] `/oplog`: window inspector (oldest/latest ts, retention seconds, entry count); load older / live tail
- [ ] `/changestream/{scope}`: WS tail of `$changeStream` at coll/db/cluster scope; resume-token copy button
- [ ] Tests

### Slice 7 — Ad-hoc query console

- [ ] `/console`: tabs for find / aggregate / runCommand
- [ ] Recent queries persisted in `~/.secantus/admin.db` (sqlite, per-URI)
- [ ] Result viewer reuses the document viewer's pagination
- [ ] Tests

### Slice 8 — Cursors & connections

- [ ] Add `secantusAdmin.cursors` and `secantusAdmin.connections` commands in `commands.py` (read-only views over `CursorRegistry` and the connection thread map)
- [ ] `/cursors`, `/connections` UI tables with kill actions
- [ ] Tests

### Slice 9 — Profiler (depends on Slice 0)

- [ ] `profile` command: `{profile: 0|1|2, slowms: N}` per DB, persisted in `admin.system.profileSettings`-style doc
- [ ] Dispatch path times each op; on level-2 or `dur >= slowms`, write entry to `<db>.system.profile` (auto-created capped, default 1 MB)
- [ ] Profile-doc shape matches mongod (`op`, `ns`, `command`, `millis`, `planSummary`, `keysExamined`, `docsExamined`, `nreturned`, `ts`, `client`)
- [ ] `/profiler`: settings form + paged entry list filtered by ns/op/min-duration
- [ ] Tests

### Slice 10 — Maintenance

- [ ] `/maintenance`: buttons for `prune_oplog`, `prune_ttl`, force WT checkpoint, drop coll/db
- [ ] Typed-confirmation modal ("type the collection name to confirm")
- [ ] Tests

### Slice 11 — Schema sampler, geo viewer, logs, settings

- [ ] Schema sampler: sample N docs, infer field paths/types/cardinality/null-rate
- [ ] Geo viewer: Leaflet map for collections with a 2dsphere index; render first N matches as GeoJSON
- [ ] Logs viewer: `getLog` results + tail-stream via WS (confirm/add `getLog`)
- [ ] Settings: saved connections in sqlite, refresh intervals, dark/light synced with website theme
- [ ] Tests

### Slice 12 — Backup / restore

- [ ] `mongodump`/`mongorestore` subprocess path (preflight checks tools on PATH)
- [ ] Native "WT checkpoint → tar" path
- [ ] UI form + progress stream
- [ ] Tests

## Documentation

- [ ] `docs/admin.md`: install (`pip install 'secantusdb[admin]'`), launch, screenshots, security model, slice-by-slice feature list
- [ ] README "Admin UI" section + screenshot
- [ ] Update `tasks/backlog.md` for anything still stubbed

## Release

- [ ] Bump version on first user-visible release
- [ ] Website blog post on cut

## Lessons / surprises

(Capture here as we go; promote to `tasks/lessons.md` if generally applicable.)

## Review

(Fill in after each slice merges to main.)
