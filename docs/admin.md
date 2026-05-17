# Admin web UI

SecantusDB ships an optional local web UI — a FastAPI app served behind
a [pywebview](https://pywebview.flowrl.com) window — for browsing
collections, watching live metrics, managing users, tailing change
streams, profiling slow queries, running maintenance, and taking
backups against any SecantusDB (or any MongoDB-wire-compatible) server
the user already has running.

It's **dev-tool shaped**, not a production console. The window connects
over loopback to one target server per launch, gates HTTP access with a
fixed local token, and never makes outbound network calls of its own.

```{contents}
:local:
:depth: 2
```

## Install

The UI lives behind an optional extra so it doesn't pull a FastAPI /
uvicorn / pywebview dependency closure into the base wheel:

```bash
pip install 'secantusdb[admin]'
```

The extra brings in `fastapi`, `uvicorn[standard]`, `jinja2`, `httpx`,
`pywebview`, and `python-multipart`. Static assets (HTMX, Alpine,
Chart.js, Leaflet) are vendored in the package — no CDN is contacted
at runtime.

## Launch

There are three equivalent ways to start the UI; all of them invoke
the same `secantus.admin.cli:main` entry point.

### Console script

```bash
secantusdb-admin --uri mongodb://127.0.0.1:27017
```

### Module

```bash
python -m secantus.admin --uri mongodb://127.0.0.1:27017
```

### Invoke task

From a checkout:

```bash
uv run python -m invoke admin --uri mongodb://127.0.0.1:27017
```

Each opens a pywebview window pointed at the local FastAPI app and
holds the process open until you close the window (or hit `Ctrl-C`).

### CLI flags

| Flag | Default | Notes |
|---|---|---|
| `--uri` | `mongodb://127.0.0.1:27017` | Target server. Any URI a `pymongo.MongoClient` accepts works — including credentials for an `--auth` server (`mongodb://user:pass@host:port/?authSource=admin`). |
| `--port` | `0` (OS-assigned) | Local HTTP port for the FastAPI app. Bound to `127.0.0.1` only. |
| `--no-window` | off | Run headless without opening pywebview. Used in CI and tests; the same URL works in any browser. |
| `--token` | unset | Override the auth token for this launch. |
| `--token-path` | `~/.secantus/admin-token` | File to read or persist the default token in. |

### Headless / behind your own browser

`--no-window` lets you point a browser at the UI yourself — useful on
remote dev boxes:

```bash
secantusdb-admin --uri mongodb://127.0.0.1:27017 --port 8765 --no-window
# open http://127.0.0.1:8765/?t=<token>  in a browser
```

The token is printed to the launcher log. Or pass `--token <fixed>` so
you can bookmark a stable URL.

## Security model

The UI is loopback-only and gated by a token. There is no separate
admin-UI auth layer — the assumption is that anyone running the
process on a machine has full local trust.

* The FastAPI app binds to `127.0.0.1` and refuses non-loopback
  bindings. Forwarding the port is up to you.
* Every non-`/healthz`, non-`/static/*` request must carry the token.
  The middleware accepts it via `?t=<token>` query parameter (used on
  first page load), an `X-Admin-Token` header (HTMX / fetch calls), or
  a `secantus-admin-token` cookie set on first successful auth
  (HttpOnly, SameSite=Strict, scoped to `/`).
* WebSocket routes (`/ws/metrics`, `/ws/changes/*`) accept the token
  via query string or cookie — browsers can't send custom headers on
  the WS handshake.
* The token is **persistent across launches** by default. First launch
  generates a 32-byte URL-safe token and writes it to
  `~/.secantus/admin-token` (mode `0600`). Subsequent launches reuse
  it, so a bookmarked window URL keeps working.
* When the target server runs with `--auth`, embed the credentials in
  the URI: `mongodb://alice:secret@127.0.0.1:27017/?authSource=admin`.
  The UI's pymongo client uses them like any other tool would.

## Page tour

The sidebar lists every page. Each is a thin client over a real wire
command — there's no parallel data store, no schema-shadow inside the
admin app.

### Dashboard (`/`)

KPI tiles (uptime, current / total connections, total commands, wire
requests) plus four sparklines (insert / query / update / delete
ops/sec) driven by a 1 Hz WebSocket from `/ws/metrics`. The server
samples `serverStatus` every second, computes per-tick deltas, and
broadcasts to subscribed clients. Reconnects automatically with
exponential backoff capped at 15 seconds.

### Databases & collections (`/db`, `/db/{db}`, `/db/{db}/{coll}`)

* `/db` — table of databases via `listDatabases`.
* `/db/{db}` — collections in a database with `count`, `dataSize`,
  `indexSize`, and a `capped` badge for capped collections.
* `/db/{db}/{coll}` — paginated document viewer. Filter is parsed via
  `bson.json_util.loads` so Extended JSON like `{"$oid": "..."}`
  works. Sort is `_id` ascending or descending; pagination is a
  stateless skip-ID cursor (`_id > last_seen`). Per-row Edit and
  Delete buttons open typed-confirmation modals — Edit replaces the
  whole document via `replace_one` (with `_id` immutability enforced
  server-side); Delete requires typing the collection name to confirm.

The collection viewer's page-actions row links to four narrower
inspectors: **Indexes**, **Explain plan**, **Schema**, and **Geo**.

### Indexes & explain (`/db/{db}/{coll}/indexes`, `/explain`)

* Index list with badges for `unique`, `sparse`, `multikey`, `partial`,
  `TTL Ns`, `2dsphere`, `2d`, `hashed`. Create form takes a key spec
  (Extended JSON) plus optional `unique` / `sparse` / partial filter
  expression / TTL. Drop button gated by typed-confirm; the `_id_`
  index can never be dropped.
* Explain visualizer renders the `winningPlan` as a depth-indented
  tree — `FETCH > IXSCAN { indexName, keyPattern, direction }` when an
  index covers the query, `COLLSCAN` (red-coded) otherwise. Multi-field
  sort acceleration shows up naturally as the IXSCAN row's direction
  marker.

### Schema sampler (`/db/{db}/{coll}/schema`)

Samples up to 1000 documents via `$sample`, walks every dotted path
(including arrays of objects), and renders presence percentage, BSON
type histogram, and the ten most common scalar values per field. Use
this on an unfamiliar collection to size up its shape without
guessing.

### Geo viewer (`/db/{db}/{coll}/geo`)

For collections with a `2dsphere` or `2d` index, drops the first 200
sampled documents onto a Leaflet map (OpenStreetMap tiles). GeoJSON
`Point` / `Polygon` / `LineString` and legacy `[lng, lat]` pairs are
both rendered; `_id` shows in the popup. Empty-state links the user
to the indexes page when no geo index exists.

### Users + roles (`/users`, `/roles`)

`/users` lists users on a "home database" (defaults to `admin`,
switchable via the inline picker). Per-row actions:

* **Password** — typed-confirm modal that runs `updateUser` with the
  new `pwd`.
* **Roles** — checkbox grid of every built-in role bound to the
  current home db or `admin`; submit diffs against the user's
  current bindings and emits `grantRolesToUser` /
  `revokeRolesFromUser` as needed.
* **Drop** — typed-confirm modal (must type the username) that runs
  `dropUser`.

`/roles` is read-only — the table reflects `secantus.rbac.BUILT_IN_ROLES`
exactly. Each row shows the role's action set as inline pills plus
flag badges (`any_db`, `cluster`, `admin_only`). Custom roles are
deferred — see [Compatibility](compatibility.md).

### Change-stream tail (`/changestream`)

Pick a scope (cluster, db, coll), hit Watch, and a WebSocket-driven
event log fills the page. Each event renders as a card with the
`operationType` badge, the namespace, an Extended-JSON pre block, and
a "Copy resume token" button that pushes the resume token to the
clipboard. A DDL filter toggle drops `create` / `drop` /
`createIndexes` / `dropIndexes` / `rename` / `modify` / `invalidate`
events when off.

The bridge from pymongo's sync `ChangeStream` to async runs the cursor
in a thread (`asyncio.to_thread(stream.try_next)`); the cursor is
closed cleanly on disconnect.

### Query (`/query`)

Three Alpine-toggled tabs that ride a single `queryPage` component
in `static/js/query.js`:

* **find** — db, collection, filter / sort / projection (Extended
  JSON), limit clamped to ≤ 200.
* **aggregate** — db, collection, pipeline (Extended JSON array),
  limit clamped to ≤ 1000.
* **runCommand** — db, command (Extended JSON object).

Results render as Extended-JSON pre blocks. Every successful submit
is recorded in a per-URI history at `~/.secantus/admin.db` (SQLite,
capped at 50 entries per URI). Click a "Recent" row to repopulate the
active form via `fetch(/query/history/{id})`.

### Insert (`/insert`)

A dedicated page for adding documents — a sibling to Query rather
than a fourth tab, because the input shape (one or more documents,
no filter / sort / projection) doesn't fit the find / aggregate /
runCommand mould.

* **db / coll** — datalist-backed inputs reusing the same
  `listDatabases` / `listCollections` sources as Query.
* **Document(s)** — Extended-JSON textarea. Accepts a single
  object `{...}` or a JSON array `[{...}, {...}]`; both shapes
  route through `insertMany`.

The response renders inline below the form with the inserted
`_id`s.

### Server (`/server`)

Target-switching plus the embedded server's lifecycle:

* **Embedded SecantusDB server** — start / stop the in-process
  server. Optional `storage_path` selects an on-disk directory;
  blank defaults to a per-launch tempdir. Starting it switches
  the admin app's target to the embedded URI automatically.
* **Switch to a new target** — accepts any URI the CLI's
  `--uri` flag would accept, credentials inline.
  Open WebSocket clients (dashboard metrics, change-stream tail)
  keep their queues and start streaming from the new server on
  the next tick.
* **Recently used** — table of prior targets. Per-row Switch
  / Forget actions. The "current" target is tagged with a badge
  and has no actions.

### Connections + cursors (`/connections`, `/cursors`)

Both views read from `currentOp`'s `inprog` array.

* `/connections` — read-only table (conn_id, client `host:port`,
  user, last op, active flag, connected_at). Connection-kill is
  deferred until SecantusDB grows a `killOp` command — see
  [Compatibility](compatibility.md).
* `/cursors` — table of live tailable / batched cursors with badges
  for `tailable` and `awaitData`. Per-row Kill button gated by
  typed-confirmation modal that issues `killCursors` over the wire.

### Profiler (`/profiler`)

Per-database settings form drives the `profile` command:

* **Level** — `0` off, `1` slow ops only (`millis ≥ slowms`),
  `2` every op.
* **slowms** — slow-op threshold in milliseconds.
* **sampleRate** — `0.0`–`1.0` (level 1+ records `sampleRate`
  fraction of qualifying ops).

Below the form, a recent-50 entries table reads
`<db>.system.profile`. Each row shows op type, namespace, latency
in ms, ok flag, optional user, and the full Extended-JSON profile
entry in a pre block.

The profile collection is auto-created as a 10 MB capped collection
on first profile-write. The dispatch path skips profiling against
`system.profile` itself (any op, not just inserts), handshake / cursor
continuation, SCRAM rounds, and the `profile` command — so reads of
the profile collection don't generate more profile entries.

### Maintenance (`/maintenance`)

Five buttons in two zones:

* **Safe** — Force checkpoint (`fsync`), Prune oplog
  (`secantusAdmin.pruneOplog`), Prune TTL
  (`secantusAdmin.pruneTtl`). Each POSTs to its endpoint and
  re-renders the page with a flash banner.
* **Danger zone** — per-database Drop button + a Drop collection
  form. Both open typed-confirm modals that require the user to type
  the target name verbatim before the wire command is issued.

### Logs (`/logs`)

Fetches `getLog("global")` every 2 seconds via HTMX polling. The
server-side ring buffer (`secantus.logbuf.LogBuffer`) holds the last
5000 lines.

### Backup (`/backup`)

Lists existing backups under `~/.secantus/backups/` and offers a "Run
mongodump now" button plus per-row Restore. Both shell out to the
official `mongodump` / `mongorestore` binaries — SecantusDB speaks
the same wire protocol as `mongod`, so the same tools work
unchanged. A preflight check looks for both binaries on PATH and
disables the action buttons with an "install mongo-tools" hint when
they're missing. The restore endpoint guards against directory
traversal in the form value (`/` and `..` are rejected with an
"invalid backup name" flash).

## Files written to disk

The UI persists three small artifacts in `~/.secantus/`:

| Path | Format | Purpose |
|---|---|---|
| `~/.secantus/admin-token` | UTF-8 string | URL-safe token, mode `0600`. Generated on first launch. |
| `~/.secantus/admin.db` | SQLite | Console query history (per-URI ring, 50 entries each). |
| `~/.secantus/backups/<UTC-stamp>/` | mongodump output | One directory per `mongodump` run. |

Everything else lives in process memory. The token file is the only
thing you need to remove if you want a clean slate (`rm
~/.secantus/admin-token`); next launch will generate a fresh one.

## Limitations

These are the gaps a /CONNECTING_USER_NEEDS_TO_KNOW level. Full
backlog at `tasks/backlog.md`.

* **Connection-kill** isn't implemented — `/connections` is read-only.
  Needs `killOp` (interruptible commands at the dispatch layer) on
  the SecantusDB side.
* **Oplog window inspector** is deferred. The data source is now in
  place — `local.oplog.rs` queryable from any pymongo client (see
  [Change streams → Querying the oplog directly](change-streams.md#querying-the-oplog-directly))
  — so the admin UI page can land as a follow-up using the standard
  collection-viewer pattern. Until then, `/changestream` shows events
  from "now" and a pymongo shell against `client.local.oplog_rs.find()`
  is the way to browse historical entries.
* **Native WT-checkpoint backup** isn't exposed. Only mongodump-driven
  backup ships today. The native path needs a server-side
  `secantusAdmin.backupArchive` command (the admin app talks only over
  the wire and doesn't know the server's `storage_path`).
* **Saved-connections / settings page** is deferred. The CLI takes a
  single `--uri` per launch, so saved bookmarks are low-value until
  the launcher gains hot-swap support.
* **Profiler entries** populate the mongod-faithful subset (`ts`,
  `op`, `ns`, `command`, `millis`, `ok`, `client`, `user`, `errMsg` /
  `errCode`). The `planSummary` / `keysExamined` / `docsExamined` /
  `nreturned` fields would need post-handler stats plumbing and are
  not included.

## Programmatic use

For tests or embedded scenarios, construct the FastAPI app directly
via `secantus.admin.create_app`:

```python
from httpx import ASGITransport, AsyncClient
from secantus import SecantusDBServer
from secantus.admin import create_app

with SecantusDBServer(port=0, storage_path=":memory:") as srv:
    app = create_app(
        mongo_uri=srv.uri,
        token="testtoken",
        history_path="/tmp/history.db",
        backup_root="/tmp/backups",
    )
    async with AsyncClient(
        transport=ASGITransport(app=app),
        base_url="http://test",
    ) as c:
        r = await c.get("/healthz")
        assert r.status_code == 200
```

`create_app` accepts `history_path` and `backup_root` overrides so
tests don't pollute `~/.secantus/`.
