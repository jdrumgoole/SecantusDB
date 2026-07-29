#!/usr/bin/env python
"""Capture the admin web UI's documentation screenshots.

Boots a throwaway SecantusDB on a fixed loopback port, seeds it with a
small fictional dataset, starts the admin FastAPI app headless, and
drives Playwright over every page in the UI — clicking, filling and
submitting where a static ``GET`` would only show an empty form. The
PNGs land in ``docs/screenshots/``, which is the single source for all
three surfaces that show them: ``docs/admin.md`` references them directly,
``README.md`` links them on raw.githubusercontent, and the Pelican build
copies its four out at build time (``website/tasks.py`` SITE_SCREENSHOTS).

Everything the shots contain is synthetic: fictional customers,
``user@example.com`` addresses, and a storage path under a temp dir that
is rewritten to ``/var/lib/secantus`` in the DOM before the shutter
fires. No developer home directory, hostname or real data appears in a
committed image.

Regenerate on every release (the ``secantusdb-release`` skill lists this
as a mandatory pre-flight step)::

    uv run python -m invoke admin-screenshots

or directly, for one page while iterating::

    uv run python scripts/admin_screenshots.py --only dashboard --headed
"""

from __future__ import annotations

import argparse
import contextlib
import datetime as dt
import shutil
import socket
import sys
import tempfile
import threading
import time
from collections.abc import Callable, Iterator
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parent.parent
# Deliberately not under docs/_static/: that whole directory is copied into
# every Sphinx build verbatim, and Sphinx *also* copies each referenced
# image into _images/ — so 5 MB of screenshots there ships twice.
DEFAULT_OUT_DIR = REPO_ROOT / "docs" / "screenshots"


# Fixed so the "connected to" badge in every page header renders the same
# string across regenerations — an ephemeral port would rewrite all 22
# images on every run. 27018 is the project's ad-hoc convention (mongod's
# alternate port), see CLAUDE.md "Tooling".
DEFAULT_SERVER_PORT = 27018

# A fixed token keeps the URL bar out of the equation; the window never
# shows it, but a stable value makes reruns reproducible.
ADMIN_TOKEN = "screenshot-token"

# The seeded storage directory is a temp path; show users the path they
# would actually see in a deployment instead.
DISPLAY_STORAGE_PATH = "/var/lib/secantus"


# --------------------------------------------------------------------------
# Demo data
# --------------------------------------------------------------------------

# Fictional. Deliberately not derived from any real dataset, and every
# address is on example.com so a committed screenshot can never leak a
# working mailbox.
_CUSTOMERS: tuple[dict[str, Any], ...] = (
    {"_id": 1, "name": "Ada Byron", "email": "ada@example.com", "tier": "gold", "city": "London"},
    {
        "_id": 2,
        "name": "Alan Turing",
        "email": "alan@example.com",
        "tier": "gold",
        "city": "London",
    },
    {
        "_id": 3,
        "name": "Grace Hopper",
        "email": "grace@example.com",
        "tier": "silver",
        "city": "New York",
    },
    {
        "_id": 4,
        "name": "Edsger Dijkstra",
        "email": "edsger@example.com",
        "tier": "silver",
        "city": "Austin",
    },
    {
        "_id": 5,
        "name": "Barbara Liskov",
        "email": "barbara@example.com",
        "tier": "bronze",
        "city": "Boston",
    },
    {
        "_id": 6,
        "name": "Ken Thompson",
        "email": "ken@example.com",
        "tier": "bronze",
        "city": "Berkeley",
    },
)

_PRODUCTS: tuple[dict[str, Any], ...] = (
    {"_id": "SKU-1001", "name": "Mechanical keyboard", "price": 129.0, "tags": ["input", "desk"]},
    {"_id": "SKU-1002", "name": "27-inch monitor", "price": 349.0, "tags": ["display", "desk"]},
    {"_id": "SKU-1003", "name": "USB-C dock", "price": 189.5, "tags": ["hub", "desk"]},
    {"_id": "SKU-1004", "name": "Noise-cancelling headset", "price": 219.0, "tags": ["audio"]},
    {"_id": "SKU-1005", "name": "Standing desk mat", "price": 59.0, "tags": ["desk", "comfort"]},
)

# Lat/lng pairs for well-known public places — no home addresses.
_STORES: tuple[dict[str, Any], ...] = (
    {"_id": "LON", "name": "London flagship", "lng": -0.1276, "lat": 51.5072, "staff": 24},
    {"_id": "NYC", "name": "New York downtown", "lng": -74.0060, "lat": 40.7128, "staff": 31},
    {"_id": "SFO", "name": "San Francisco market", "lng": -122.4194, "lat": 37.7749, "staff": 18},
    {"_id": "BER", "name": "Berlin Mitte", "lng": 13.4050, "lat": 52.5200, "staff": 12},
    {"_id": "TOK", "name": "Tokyo Shibuya", "lng": 139.6917, "lat": 35.6895, "staff": 27},
    {"_id": "SYD", "name": "Sydney harbour", "lng": 151.2093, "lat": -33.8688, "staff": 15},
)

_STATUSES = ("placed", "picking", "shipped", "delivered", "returned")

# Fixed epoch so re-runs produce the same document contents. Not "now" —
# a moving base date would rewrite every order on every regeneration.
_EPOCH = dt.datetime(2026, 1, 6, 9, 0, 0, tzinfo=dt.timezone.utc)


def _orders(count: int = 240) -> Iterator[dict[str, Any]]:
    """Deterministic order documents — no RNG, so reruns are byte-stable."""
    for i in range(count):
        customer = _CUSTOMERS[i % len(_CUSTOMERS)]
        product = _PRODUCTS[i % len(_PRODUCTS)]
        qty = (i % 4) + 1
        yield {
            "order_no": f"ORD-{2026_0000 + i}",
            "customer_id": customer["_id"],
            "customer_name": customer["name"],
            "status": _STATUSES[i % len(_STATUSES)],
            "placed_at": _EPOCH + dt.timedelta(hours=i * 3),
            "items": [
                {
                    "sku": product["_id"],
                    "name": product["name"],
                    "qty": qty,
                    "price": product["price"],
                }
            ],
            "total": round(product["price"] * qty, 2),
            "shipping": {
                "city": customer["city"],
                "country": "GB" if customer["city"] == "London" else "US",
            },
        }


def seed(uri: str) -> None:
    """Populate the throwaway server with the fictional shop dataset."""
    from pymongo import ASCENDING, DESCENDING, GEOSPHERE, MongoClient

    client: MongoClient[dict[str, Any]] = MongoClient(uri)
    try:
        shop = client["shop"]
        shop["customers"].insert_many(list(_CUSTOMERS))
        shop["products"].insert_many(list(_PRODUCTS))
        shop["orders"].insert_many(list(_orders()))
        shop["stores"].insert_many(
            [
                {
                    "_id": s["_id"],
                    "name": s["name"],
                    "staff": s["staff"],
                    "location": {"type": "Point", "coordinates": [s["lng"], s["lat"]]},
                }
                for s in _STORES
            ]
        )

        # A spread of index shapes so the Indexes page shows every badge
        # the template can render (compound, unique, sparse, partial, TTL,
        # 2dsphere) rather than a lonely `_id_`.
        shop["orders"].create_index(
            [("status", ASCENDING), ("placed_at", DESCENDING)], name="status_placed"
        )
        shop["orders"].create_index([("order_no", ASCENDING)], name="order_no_unique", unique=True)
        shop["orders"].create_index([("customer_id", ASCENDING)], name="customer_id")
        shop["orders"].create_index(
            [("total", DESCENDING)],
            name="high_value",
            partialFilterExpression={"total": {"$gte": 200}},
        )
        shop["customers"].create_index([("email", ASCENDING)], name="email_unique", unique=True)
        shop["customers"].create_index([("tier", ASCENDING)], name="tier")
        shop["products"].create_index([("tags", ASCENDING)], name="tags_multikey")
        shop["stores"].create_index([("location", GEOSPHERE)], name="location_2dsphere")

        # A second database so the Databases page isn't a single row.
        analytics = client["analytics"]
        analytics["page_views"].insert_many(
            [
                {
                    "path": p,
                    "views": v,
                    "day": _EPOCH + dt.timedelta(days=i),
                }
                for i, (p, v) in enumerate(
                    (("/", 5120), ("/docs", 2311), ("/pricing", 964), ("/blog", 1780))
                )
            ]
        )
        analytics["sessions"].insert_many(
            [{"session": f"s-{i:03d}", "duration_s": 30 + i * 7} for i in range(50)]
        )

        # Users + roles so those two pages have rows. Fictional service
        # accounts, never a person's name.
        admin_db = client["admin"]
        for username, roles in (
            ("app_reader", [{"role": "read", "db": "shop"}]),
            ("app_writer", [{"role": "readWrite", "db": "shop"}]),
            ("ops", [{"role": "clusterMonitor", "db": "admin"}]),
        ):
            with contextlib.suppress(Exception):
                admin_db.command("createUser", username, pwd="not-a-real-password", roles=roles)

        # Profiling on, then some traffic, so the Profiler page has rows.
        with contextlib.suppress(Exception):
            shop.command("profile", 2)
        for status in _STATUSES:
            shop["orders"].count_documents({"status": status})
        list(shop["orders"].find({"total": {"$gte": 200}}).sort("placed_at", DESCENDING).limit(20))
        list(
            shop["orders"].aggregate(
                [
                    {
                        "$group": {
                            "_id": "$status",
                            "orders": {"$sum": 1},
                            "revenue": {"$sum": "$total"},
                        }
                    },
                    {"$sort": {"revenue": -1}},
                ]
            )
        )

        # A handful of post-index writes so the Oplog page shows `i`/`u`/`d`
        # entries rather than a wall of inserts.
        shop["orders"].update_many({"status": "placed"}, {"$set": {"flagged": False}})
        shop["orders"].update_one({"order_no": "ORD-20260000"}, {"$set": {"status": "shipped"}})
        shop["products"].delete_one({"_id": "SKU-1005"})
        shop["products"].insert_one(
            {
                "_id": "SKU-1005",
                "name": "Standing desk mat",
                "price": 59.0,
                "tags": ["desk", "comfort"],
            }
        )

        # Profiling off again now that ``system.profile`` has rows. Left on,
        # every later operation writes a profile document, and those inserts
        # drown the oplog and change-stream pages in traffic the reader
        # didn't cause and can't interpret.
        with contextlib.suppress(Exception):
            shop.command("profile", 0)
    finally:
        client.close()


# --------------------------------------------------------------------------
# Server + admin app lifecycle
# --------------------------------------------------------------------------


def _port_is_free(host: str, port: int) -> bool:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            sock.bind((host, port))
        except OSError:
            return False
    return True


class AdminHarness:
    """Throwaway SecantusDB + headless admin app, torn down on exit."""

    def __init__(
        self,
        *,
        server_port: int,
        keep_data: bool,
        checkout: Path | None = None,
    ) -> None:
        self._server_port = server_port
        self._keep_data = keep_data
        self._checkout = checkout
        self._data_dir = Path(tempfile.mkdtemp(prefix="secantus-shots-"))
        self._backup_root = self._data_dir / "backups"
        self._server: Any = None
        self._uvicorn: Any = None
        self._thread: threading.Thread | None = None
        self.app: Any = None
        self.base_url = ""

    @property
    def data_dir(self) -> Path:
        return self._data_dir

    @property
    def server_uri(self) -> str:
        """URI of the throwaway server the admin app is pointed at."""
        return str(self._server.uri)

    def start(self) -> None:
        from secantus import SecantusDBServer

        if not _port_is_free("127.0.0.1", self._server_port):
            raise SystemExit(
                f"127.0.0.1:{self._server_port} is already in use. Stop whatever is "
                f"listening there, or pass --server-port to pick another. The port is "
                f"fixed on purpose so the header badge is identical across reruns."
            )
        self._server = SecantusDBServer(
            port=self._server_port,
            storage_path=str(self._data_dir / "storage"),
            # A slow heartbeat keeps the oplog / change-stream pages alive
            # without a wall of noop rows in the shot.
            noop_heartbeat_seconds=10.0,
        )
        self._server.start()
        seed(self._server.uri)
        self._start_admin(self._server.uri)

    def _start_admin(self, uri: str) -> None:
        import uvicorn

        from secantus.admin import create_app

        self.app = create_app(
            mongo_uri=uri,
            token=ADMIN_TOKEN,
            history_path=self._data_dir / "history.db",
            backup_root=self._backup_root,
        )
        if self._checkout is not None:
            self._serve_from_checkout(self._checkout)
        config = uvicorn.Config(
            self.app,
            host="127.0.0.1",
            port=0,
            log_level="warning",
            lifespan="on",
        )
        self._uvicorn = uvicorn.Server(config)
        self._thread = threading.Thread(target=self._uvicorn.run, daemon=True, name="admin-uvicorn")
        self._thread.start()

        deadline = time.monotonic() + 30.0
        while time.monotonic() < deadline:
            servers = getattr(self._uvicorn, "servers", None)
            if self._uvicorn.started and servers:
                port = servers[0].sockets[0].getsockname()[1]
                self.base_url = f"http://127.0.0.1:{port}"
                return
            time.sleep(0.05)
        raise SystemExit("the admin app did not start within 30s")

    def _serve_from_checkout(self, checkout: Path) -> None:
        """Render templates and serve /static from a source checkout.

        A git worktree normally borrows the main checkout's venv, whose
        editable install resolves ``secantus`` to the *other* tree — so UI
        edits made in the worktree are invisible to the app under capture,
        and the screenshots quietly document the wrong code. Both asset
        roots are redirectable at runtime: every router renders through
        ``app.state.templates_dir``, and the ``/static`` mount is an
        ordinary route whose sub-app can be swapped.
        """
        from fastapi.staticfiles import StaticFiles

        admin_pkg = checkout / "src" / "secantus" / "admin"
        templates = admin_pkg / "templates"
        static = admin_pkg / "static"
        for path in (templates, static):
            if not path.is_dir():
                raise SystemExit(f"--from-checkout: {path} does not exist")
        self.app.state.templates_dir = str(templates)
        for route in self.app.routes:
            if getattr(route, "name", None) == "static":
                route.app = StaticFiles(directory=static)
                break
        else:  # pragma: no cover - the mount is created in create_app
            raise SystemExit("--from-checkout: no /static mount found on the app")
        print(f"serving templates + static from {checkout}")

    def make_backups(self) -> None:
        """Take two checkpoint archives so the Backup page lists prior runs.

        ``list_backups`` walks the backup root for directories (mongodump
        output) and ``*.tar.gz`` files (native ``secantusAdmin.backupArchive``
        output); the native command is the one that needs no external
        ``mongodump`` binary, so it's what the shot uses. Failures are
        raised, not suppressed — a swallowed error here is how the page
        ends up photographed empty.
        """
        from pymongo import MongoClient

        self._backup_root.mkdir(parents=True, exist_ok=True)
        client: MongoClient[dict[str, Any]] = MongoClient(self._server.uri)
        try:
            for name in ("nightly-2026-01-06.tar.gz", "nightly-2026-01-07.tar.gz"):
                client["admin"].command(
                    "secantusAdmin.backupArchive",
                    outputPath=str(self._backup_root / name),
                )
        finally:
            client.close()

    def stop(self) -> None:
        if self._uvicorn is not None:
            self._uvicorn.should_exit = True
        if self._thread is not None:
            self._thread.join(timeout=10.0)
        if self.app is not None:
            with contextlib.suppress(Exception):
                self.app.state.mongo.close()
        if self._server is not None:
            with contextlib.suppress(Exception):
                self._server.stop()
        if self._keep_data:
            print(f"data kept at {self._data_dir}")
        else:
            shutil.rmtree(self._data_dir, ignore_errors=True)


# --------------------------------------------------------------------------
# Page interactions
# --------------------------------------------------------------------------


def _settle(page: Any, ms: int = 400) -> None:
    """Let HTMX swaps / Alpine renders / Chart.js animations finish."""
    page.wait_for_timeout(ms)


@contextlib.contextmanager
def _background_load(uri: str) -> Iterator[None]:
    """Drive continuous CRUD at the server for the duration of the block.

    The dashboard's tiles and charts are rate meters fed by a 1 Hz
    ``serverStatus`` sampler — against an idle server every number is a
    zero and every chart is a flat line, which documents nothing. This
    keeps real traffic flowing so the shot shows the page doing its job.
    """
    from pymongo import MongoClient

    stop = threading.Event()

    def _work() -> None:
        client: MongoClient[dict[str, Any]] = MongoClient(uri)
        try:
            coll = client["analytics"]["sessions"]
            i = 0
            while not stop.is_set():
                i += 1
                coll.insert_one({"session": f"live-{i:05d}", "duration_s": i % 300})
                # A real find, not just count_documents: counts land on the
                # `command` opcounter, so without this the dashboard's
                # Queries sparkline sits flat at zero.
                list(coll.find({"duration_s": {"$gte": 100}}).limit(5))
                coll.update_one({"session": f"live-{i:05d}"}, {"$set": {"seen": True}})
                if i % 3 == 0:
                    coll.delete_one({"session": f"live-{i - 2:05d}"})
                stop.wait(0.02)
        finally:
            client.close()

    workers = [threading.Thread(target=_work, daemon=True, name=f"shot-load-{n}") for n in range(3)]
    for worker in workers:
        worker.start()
    try:
        yield
    finally:
        stop.set()
        for worker in workers:
            worker.join(timeout=5.0)


def _dashboard(page: Any, harness: AdminHarness) -> None:
    """Wait for the metrics socket, then let the charts fill under load."""
    page.wait_for_function(
        "() => document.querySelector('.connection-status')?.textContent.trim() === 'live'",
        timeout=15_000,
    )
    with _background_load(harness.server_uri):
        # ~1 sample/second; a dozen gives the sparklines a readable shape.
        _settle(page, 13_000)


def _run_query(page: Any) -> None:
    page.fill('form[x-show="kind === \'find\'"] input[name="db"]', "shop")
    page.fill('form[x-show="kind === \'find\'"] input[name="coll"]', "orders")
    page.fill('form[x-show="kind === \'find\'"] textarea[name="filter"]', '{"status": "shipped"}')
    page.fill('form[x-show="kind === \'find\'"] textarea[name="sort"]', '{"placed_at": -1}')
    page.click('form[x-show="kind === \'find\'"] button[type="submit"]')
    _settle(page, 1200)


def _fill_insert(page: Any) -> None:
    page.fill('input[name="db"]', "shop")
    page.fill('input[name="coll"]', "customers")
    page.fill(
        'textarea[name="docs"]',
        '{"_id": 7, "name": "Radia Perlman", "email": "radia@example.com",\n'
        ' "tier": "gold", "city": "Boston"}',
    )
    _settle(page)


def _watch_changestream(page: Any, harness: AdminHarness) -> None:
    """Start the tail, then write from a second client so events land."""
    from pymongo import MongoClient

    page.click('button:has-text("Watch")')
    _settle(page, 600)
    client: MongoClient[dict[str, Any]] = MongoClient(harness.server_uri)
    try:
        coll = client["shop"]["orders"]
        coll.insert_one({"order_no": "ORD-20269001", "status": "placed", "total": 129.0})
        coll.update_one({"order_no": "ORD-20269001"}, {"$set": {"status": "picking"}})
        coll.update_one({"order_no": "ORD-20269001"}, {"$set": {"status": "shipped"}})
        coll.delete_one({"order_no": "ORD-20269001"})
    finally:
        client.close()
    _settle(page, 1800)


def _busy_connections(page: Any, harness: AdminHarness) -> None:
    """Reload the connections table while several clients are actually busy."""
    with _background_load(harness.server_uri):
        _settle(page, 1200)
        page.reload()
        _settle(page, 1200)


def _open_cursor(page: Any, harness: AdminHarness) -> None:
    """Leave a batched cursor open so the Cursors page has a row."""
    from pymongo import MongoClient

    client: MongoClient[dict[str, Any]] = MongoClient(harness.server_uri)
    cursors = [client["shop"]["orders"].find({}, batch_size=10) for _ in range(3)]
    for cursor in cursors:
        next(cursor, None)
    page.reload()
    _settle(page, 800)
    for cursor in cursors:
        cursor.close()
    client.close()


# Create-index declares ``key: str = Form(...)``, so a fieldless POST fails
# FastAPI's validation *before* the handler runs — the app renders
# ``error.html`` (see the RequestValidationError handler in admin/app.py)
# and no index is created. Routes without a required field are no good
# here: an empty POST to ``/backup/dump`` doesn't fail at all, it runs a
# real mongodump and returns the Backup page.
_INVALID_FORM_ACTION = "/db/shop/orders/indexes"


def _submit_invalid_form(page: Any) -> None:
    """Trigger the friendly error page via a form missing a required field."""
    page.evaluate(
        """(action) => {
            const f = document.createElement('form');
            f.method = 'POST';
            f.action = action;
            document.body.appendChild(f);
            f.submit();
        }""",
        _INVALID_FORM_ACTION,
    )
    # Wait for the POST's response document, not the current one: the
    # submit navigates, and touching the old execution context afterwards
    # races the teardown.
    page.wait_for_url(f"**{_INVALID_FORM_ACTION}", wait_until="load")
    _settle(page, 400)


@dataclass(frozen=True)
class Shot:
    """One documented screenshot."""

    slug: str
    path: str
    caption: str
    # Runs after navigation, before the shutter. Takes (page, harness).
    prepare: Callable[[Any, AdminHarness], None] | None = None
    full_page: bool = False
    # Extra settle time for pages that poll (metrics, logs).
    settle_ms: int = 500
    # Where this shot is reused outside docs/admin.md: "readme" for the
    # README's hero, "website" for the secantusdb.com landing page. Neither
    # surface is written by this script — the README links the file in git,
    # and the Pelican build copies its four out of docs/screenshots/ (see
    # website/tasks.py SITE_SCREENSHOTS). These tags are what
    # tests/test_docs_screenshots.py checks both of those against.
    tags: tuple[str, ...] = field(default=())
    # Set when a failed HTTP response is the subject of the shot, so the
    # browser's "Failed to load resource" console line isn't reported as a
    # bug. Only silences resource errors — a thrown exception still fails.
    allow_http_errors: bool = False


SHOTS: tuple[Shot, ...] = (
    Shot(
        "dashboard",
        "/",
        "The dashboard: live server metrics, operation counters and per-second charts.",
        prepare=_dashboard,
        tags=("readme", "website"),
    ),
    Shot(
        "server",
        "/server",
        "The Server page: build info, target switching and the embedded-server controls.",
    ),
    Shot(
        "databases",
        "/db",
        "The Databases page: every database with its collection and size totals.",
    ),
    Shot(
        "collections",
        "/db/shop",
        "A database's collections, with document counts and storage sizes.",
    ),
    Shot(
        "collection",
        "/db/shop/orders",
        "The collection browser: paginated documents with an inline JSON viewer.",
        tags=("readme",),
    ),
    Shot(
        "indexes",
        "/db/shop/orders/indexes",
        "The Indexes page: every index with its key spec and unique / partial / multikey badges.",
    ),
    Shot(
        "explain",
        "/db/shop/orders/explain?filter=%7B%22status%22%3A%20%22shipped%22%7D"
        "&sort=%7B%22placed_at%22%3A%20-1%7D",
        "Explain: the winning plan for a filter + sort, stage by stage.",
        tags=("website",),
    ),
    Shot(
        "schema",
        "/db/shop/orders/schema",
        "The schema sampler: inferred field types and coverage across a sample of documents.",
    ),
    Shot(
        "geo",
        "/db/shop/stores/geo",
        "The geo viewer: documents from a 2dsphere-indexed collection plotted on a map.",
        settle_ms=2000,
    ),
    Shot(
        "query",
        "/query",
        "The Query page running a find, with results and saved query history.",
        prepare=lambda page, _h: _run_query(page),
        tags=("readme", "website"),
    ),
    Shot(
        "insert",
        "/insert",
        "The Insert page: paste one document or an array and write it to any collection.",
        prepare=lambda page, _h: _fill_insert(page),
    ),
    Shot("users", "/users", "The Users page: accounts on a database with their granted roles."),
    Shot("roles", "/roles", "The Roles page: every built-in role and the privileges it carries."),
    Shot(
        # Scoped to one collection rather than the default cluster scope:
        # at cluster scope the profiler's own ``system.profile`` inserts
        # crowd out the writes the shot is meant to show.
        "changestream",
        "/changestream?scope=coll&db=shop&coll=orders",
        "The change-stream tail: live insert / update / delete events with resume tokens.",
        prepare=_watch_changestream,
        tags=("website",),
    ),
    Shot(
        "connections",
        "/connections",
        "The Connections page: current clients, with a kill control per operation.",
        prepare=_busy_connections,
    ),
    Shot(
        "cursors",
        "/cursors",
        "The Cursors page: open cursors, their namespace and idle time.",
        prepare=_open_cursor,
    ),
    Shot(
        "oplog",
        "/oplog",
        "The Oplog page: recent entries with operation type, namespace and timestamp.",
    ),
    Shot(
        "profiler",
        "/profiler",
        "The Profiler page: slow operations captured by the database profiler.",
    ),
    Shot(
        "maintenance",
        "/maintenance",
        "The Maintenance page: fsync, oplog / TTL pruning and drop controls.",
    ),
    Shot("logs", "/logs", "The Logs page: a live tail of the server's log buffer.", settle_ms=1200),
    Shot(
        "backup",
        "/backup",
        "The Backup page: dumps, archives, restores and point-in-time recovery.",
    ),
    Shot(
        # Based on the page whose form the invalid POST targets. Basing it
        # on the dashboard instead meant navigating away mid-Chart-init,
        # and the torn-down resize observers logged errors that had nothing
        # to do with the page being photographed.
        "error",
        "/db/shop/orders/indexes",
        "The friendly error page: a failed action keeps the sidebar and offers a way back.",
        prepare=lambda page, _h: _submit_invalid_form(page),
        allow_http_errors=True,
    ),
)


# --------------------------------------------------------------------------
# Empty-state detection
# --------------------------------------------------------------------------

# The "nothing to show" strings the page templates render. A screenshot of
# one of these documents nothing — and it is the failure mode this harness
# is most likely to drift into, because seeding that stops populating a
# page still produces a perfectly valid-looking PNG.
_EMPTY_STATES: tuple[str, ...] = (
    "No backups yet",
    "No collections",
    "No databases",
    "No documents",
    "No entries yet",
    "No events yet",
    "No fields seen",
    "No users on this database",
)

# Pages whose empty state is the honest thing to show: the Server page
# lists previously-saved targets, and a first run legitimately has none.
_EMPTY_STATE_OK: frozenset[str] = frozenset({"server", "error"})


def _empty_state(page: Any) -> str | None:
    """Return the empty-state phrase visible on the page, if any."""
    text = page.evaluate("() => document.body.innerText")
    for phrase in _EMPTY_STATES:
        if phrase in text:
            return phrase
    return None


# --------------------------------------------------------------------------
# Anonymisation
# --------------------------------------------------------------------------

_SCRUB_JS = """
([replacements]) => {
  const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  const nodes = [];
  while (walker.nextNode()) nodes.push(walker.currentNode);
  for (const node of nodes) {
    let text = node.nodeValue;
    for (const [from, to] of replacements) {
      if (text.includes(from)) text = text.split(from).join(to);
    }
    if (text !== node.nodeValue) node.nodeValue = text;
  }
  // Same for attributes that surface paths (title=, value=, placeholder=).
  for (const el of document.querySelectorAll('[title], [value], [placeholder]')) {
    for (const attr of ['title', 'value', 'placeholder']) {
      const cur = el.getAttribute(attr);
      if (!cur) continue;
      let next = cur;
      for (const [from, to] of replacements) {
        if (next.includes(from)) next = next.split(from).join(to);
      }
      if (next !== cur) el.setAttribute(attr, next);
    }
  }
  // Input elements hold their live value off-attribute.
  for (const el of document.querySelectorAll('input, textarea')) {
    let next = el.value;
    for (const [from, to] of replacements) {
      if (next && next.includes(from)) next = next.split(from).join(to);
    }
    if (next !== el.value) el.value = next;
  }
}
"""


def _scrub(page: Any, harness: AdminHarness) -> None:
    """Rewrite machine-specific strings out of the DOM before capture.

    Anything that names this developer's machine — the temp storage path,
    the home directory, the auth token — becomes a neutral placeholder, so
    a committed PNG carries nothing but the fictional dataset.
    """
    replacements = [
        [str(harness.data_dir / "storage"), DISPLAY_STORAGE_PATH],
        [str(harness.data_dir), DISPLAY_STORAGE_PATH],
        [str(Path.home()), "/home/user"],
        [ADMIN_TOKEN, "<token>"],
        [socket.gethostname(), "localhost"],
    ]
    page.evaluate(_SCRUB_JS, [replacements])


# --------------------------------------------------------------------------
# Capture
# --------------------------------------------------------------------------


def capture(
    harness: AdminHarness,
    shots: tuple[Shot, ...],
    *,
    out_dir: Path,
    width: int,
    height: int,
    scale: int,
    headed: bool,
) -> tuple[list[Path], list[str]]:
    """Capture every shot; return the files written and any JS errors seen.

    The browser errors are not incidental output — driving all 22 pages in
    a real Chromium is the only JS-level exercise this UI gets, and a page
    that throws during ``init()`` still screenshots fine while being
    completely dead. Both bugs found when this harness was first written
    (Alpine loading before Chart.js, and ``init()`` running twice) showed
    up here and nowhere else, so the caller treats them as failures.
    """
    from playwright.sync_api import sync_playwright

    out_dir.mkdir(parents=True, exist_ok=True)
    written: list[Path] = []
    js_errors: list[str] = []
    empty_pages: list[str] = []
    current = ""
    allow_http = False

    def _note_console(msg: Any) -> None:
        if msg.type != "error":
            return
        if allow_http and "Failed to load resource" in msg.text:
            return
        js_errors.append(f"{current or 'startup'}: console.{msg.type}: {msg.text}")

    with sync_playwright() as pw:
        browser = pw.chromium.launch(headless=not headed)
        try:
            context = browser.new_context(
                viewport={"width": width, "height": height},
                device_scale_factor=scale,
                # Pinned so date / number rendering in the UI doesn't shift
                # with whatever locale the capturing machine happens to use.
                locale="en-GB",
                timezone_id="UTC",
                color_scheme="dark",
            )
            page = context.new_page()
            page.on("pageerror", lambda exc: js_errors.append(f"{current or 'startup'}: {exc}"))
            page.on("console", _note_console)
            # One tokened navigation seeds the cookie; every later request
            # rides that instead of putting the token in each URL.
            page.goto(f"{harness.base_url}/?t={ADMIN_TOKEN}", wait_until="load")
            _settle(page, 800)

            for shot in shots:
                current = shot.slug
                allow_http = shot.allow_http_errors
                target = f"{harness.base_url}{shot.path}"
                page.goto(target, wait_until="load")
                _settle(page, shot.settle_ms)
                if shot.prepare is not None:
                    shot.prepare(page, harness)
                empty = None if shot.slug in _EMPTY_STATE_OK else _empty_state(page)
                _scrub(page, harness)
                path = out_dir / f"admin-{shot.slug}.png"
                page.screenshot(path=str(path), full_page=shot.full_page)
                written.append(path)
                suffix = f"   ⚠ empty: {empty!r}" if empty else ""
                print(f"  {path.relative_to(REPO_ROOT)}{suffix}")
                if empty:
                    empty_pages.append(f"{shot.slug}: {empty!r}")
        finally:
            browser.close()
    return written, js_errors, empty_pages


def _parse_args(argv: list[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        prog="admin-screenshots",
        description="Capture the admin UI screenshots used by the docs, README and website.",
    )
    parser.add_argument(
        "--out",
        type=Path,
        default=DEFAULT_OUT_DIR,
        help=f"Output directory (default: {DEFAULT_OUT_DIR.relative_to(REPO_ROOT)}).",
    )
    parser.add_argument(
        "--only",
        action="append",
        default=[],
        metavar="SLUG",
        help="Capture only this page (repeatable). Default: all of them.",
    )
    parser.add_argument(
        "--server-port",
        type=int,
        default=DEFAULT_SERVER_PORT,
        help=f"Port for the throwaway SecantusDB (default: {DEFAULT_SERVER_PORT}).",
    )
    parser.add_argument("--width", type=int, default=1440, help="Viewport width (default: 1440).")
    parser.add_argument("--height", type=int, default=900, help="Viewport height (default: 900).")
    parser.add_argument(
        "--scale",
        type=int,
        default=2,
        choices=(1, 2),
        help="Device scale factor; 2 gives retina-sharp PNGs (default: 2).",
    )
    parser.add_argument("--headed", action="store_true", help="Show the browser while capturing.")
    parser.add_argument(
        "--keep-data",
        action="store_true",
        help="Leave the seeded storage directory on disk for inspection.",
    )
    parser.add_argument("--list", action="store_true", help="List the page slugs and exit.")
    parser.add_argument(
        "--from-checkout",
        type=Path,
        default=None,
        metavar="REPO",
        help=(
            "Serve templates and /static from this checkout instead of the "
            "installed package. Use it when iterating on the UI in a git "
            "worktree that borrows the main checkout's venv, where the "
            "editable install would otherwise render the other tree's code."
        ),
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = _parse_args(argv)

    if args.list:
        for shot in SHOTS:
            print(f"{shot.slug:<14} {shot.path}")
        return 0

    shots = SHOTS
    if args.only:
        wanted = set(args.only)
        unknown = wanted - {s.slug for s in SHOTS}
        if unknown:
            print(f"unknown page(s): {', '.join(sorted(unknown))}", file=sys.stderr)
            print(f"known: {', '.join(s.slug for s in SHOTS)}", file=sys.stderr)
            return 2
        shots = tuple(s for s in SHOTS if s.slug in wanted)

    try:
        from playwright.sync_api import sync_playwright  # noqa: F401
    except ModuleNotFoundError:
        print(
            "playwright is not installed. Install the screenshots extra and the browser:\n"
            "  uv sync --extra screenshots\n"
            "  uv run playwright install chromium",
            file=sys.stderr,
        )
        return 2

    harness = AdminHarness(
        server_port=args.server_port,
        keep_data=args.keep_data,
        checkout=args.from_checkout,
    )
    try:
        print(f"starting SecantusDB on 127.0.0.1:{args.server_port} and seeding demo data...")
        harness.start()
        harness.make_backups()
        print(f"admin app at {harness.base_url}; capturing {len(shots)} page(s):")
        written, js_errors, empty_pages = capture(
            harness,
            shots,
            out_dir=args.out,
            width=args.width,
            height=args.height,
            scale=args.scale,
            headed=args.headed,
        )
    except KeyboardInterrupt:
        print("\ninterrupted — shutting down", file=sys.stderr)
        return 130
    finally:
        harness.stop()

    total = sum(p.stat().st_size for p in written)
    print(f"wrote {len(written)} screenshot(s), {total / 1_000_000:.1f} MB total")

    if empty_pages:
        print(f"\n{len(empty_pages)} page(s) shot in an empty state:", file=sys.stderr)
        for entry in empty_pages:
            print(f"  {entry}", file=sys.stderr)
        print(
            "Seed data for those pages in seed() — an empty page documents nothing.",
            file=sys.stderr,
        )

    if js_errors:
        print(f"\n{len(js_errors)} JavaScript error(s) in the admin UI:", file=sys.stderr)
        for err in js_errors:
            print(f"  {err}", file=sys.stderr)
        print(
            "\nThe PNGs were still written, but a page that throws during init "
            "renders dead controls — fix the errors before committing the shots.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
