"""In-process embedded SecantusDB server controlled from the dashboard.

The admin app can spin up a real SecantusDB inside the same process so a
user with no daemon running can click one button and have a working
target. Listens on ``127.0.0.1`` on a kernel-chosen port; storage lives
at ``~/.secantus/embedded-data/`` by default but can be overridden per
launch.

**Both servers can be launched.** SecantusDB ships two independent
servers (see the project CLAUDE.md) and the dashboard offers whichever
are importable:

* ``"python"`` — ``secantus.SecantusDBServer``, always available since
  it is part of this package.
* ``"rust"`` — ``_secantus_server.RustServer``, the embedded handle
  around the Rust accept loop. Present only when the compiled extension
  is installed, so it is imported lazily and :func:`available_flavours`
  reports what the running install can actually offer.

Both expose the same three things this module needs — a ``uri``, and
start/stop lifecycle — but they differ in shape: ``SecantusDBServer`` is
constructed stopped and started with ``.start()``, whereas
``RustServer`` binds and starts its accept loop in the constructor.
:class:`EmbeddedServer` normalises that difference so the routers and
templates stay flavour-agnostic.

Lifecycle is wrapped in :class:`EmbeddedServer`:

* ``start(storage_path=None, flavour=None)`` — boots a server;
  idempotent (already-running returns the existing URI).
* ``stop()`` — shuts the server down cleanly; safe to call when not
  running.
* ``status()`` — current state for the dashboard widget.

Held on ``app.state.embedded``. Stopped from the FastAPI ``lifespan``
finally-block so the listener is closed when the admin process exits.
"""

from __future__ import annotations

import threading
from pathlib import Path
from typing import Any

DEFAULT_EMBEDDED_STORAGE = Path.home() / ".secantus" / "embedded-data"

PYTHON = "python"
RUST = "rust"
DEFAULT_FLAVOUR = PYTHON

_LABELS = {PYTHON: "Python server", RUST: "Rust server"}


def _rust_server_cls() -> type | None:
    """Return the Rust embedded-server class, or ``None`` if not installed.

    The compiled ``_secantus_server`` extension is an optional build
    artefact — a plain ``pip install secantus`` may not carry it — so a
    missing module is an expected state, not an error.
    """
    try:
        from _secantus_server import RustServer
    except ImportError:
        return None
    return RustServer


def available_flavours() -> list[dict[str, str]]:
    """Server flavours this install can actually launch, for the UI picker."""
    flavours = [{"value": PYTHON, "label": _LABELS[PYTHON]}]
    if _rust_server_cls() is not None:
        flavours.append({"value": RUST, "label": _LABELS[RUST]})
    return flavours


class EmbeddedServer:
    """Wraps an optional in-process SecantusDB server for the dashboard."""

    def __init__(self, default_storage_path: Path | str | None = None) -> None:
        self.default_storage_path = Path(default_storage_path or DEFAULT_EMBEDDED_STORAGE)
        self._server: Any | None = None
        self._storage_path: Path | None = None
        self._flavour: str | None = None
        self._lock = threading.Lock()

    def status(self) -> dict[str, Any]:
        with self._lock:
            if self._server is None:
                return {
                    "running": False,
                    "uri": None,
                    "storage_path": str(self.default_storage_path),
                    "flavour": None,
                    "flavour_label": None,
                }
            return {
                "running": True,
                "uri": self._server.uri,
                "storage_path": str(self._storage_path) if self._storage_path else None,
                "flavour": self._flavour,
                "flavour_label": _LABELS.get(self._flavour or "", ""),
            }

    def start(
        self,
        *,
        storage_path: Path | str | None = None,
        flavour: str | None = None,
    ) -> str:
        """Start the embedded server. Idempotent — returns the existing URI
        if one is already running (the running flavour wins; stop first to
        switch)."""
        with self._lock:
            if self._server is not None:
                return self._server.uri
            kind = (flavour or DEFAULT_FLAVOUR).strip().lower()
            if kind not in (PYTHON, RUST):
                raise ValueError(f"unknown server flavour: {flavour!r}")
            path = Path(storage_path or self.default_storage_path)
            path.mkdir(parents=True, exist_ok=True)
            srv = self._boot(kind, path)
            self._server = srv
            self._storage_path = path
            self._flavour = kind
            return srv.uri

    @staticmethod
    def _boot(kind: str, path: Path) -> Any:
        """Construct and start one server, normalising the two lifecycles."""
        if kind == RUST:
            rust_cls = _rust_server_cls()
            if rust_cls is None:
                raise RuntimeError(
                    "The Rust server extension (_secantus_server) is not installed "
                    "in this environment. Build it with `invoke rust-server-build`, "
                    "or start the Python server instead."
                )
            # RustServer binds the socket and starts its accept loop in
            # the constructor — there is no separate .start().
            return rust_cls(port=0, storage_path=str(path))

        from secantus import SecantusDBServer

        srv = SecantusDBServer(port=0, storage_path=str(path))
        srv.start()
        return srv

    def stop(self) -> None:
        """Stop the embedded server. No-op when not running."""
        with self._lock:
            if self._server is None:
                return
            try:
                self._server.stop()
            finally:
                self._server = None
                self._storage_path = None
                self._flavour = None


__all__ = [
    "EmbeddedServer",
    "DEFAULT_EMBEDDED_STORAGE",
    "available_flavours",
    "PYTHON",
    "RUST",
    "DEFAULT_FLAVOUR",
]
