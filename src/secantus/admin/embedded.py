"""In-process embedded SecantusDB server controlled from the dashboard.

The admin app can spin up a real ``SecantusDBServer`` inside the same
Python process so a user with no daemon running can click one button
and have a working target. Listens on ``127.0.0.1`` on a kernel-chosen
port; storage lives at ``~/.secantus/embedded-data/`` by default but
can be overridden per launch.

Lifecycle is wrapped in :class:`EmbeddedServer`:

* ``start(storage_path=None)`` — boots a fresh ``SecantusDBServer``
  thread; idempotent (already-running returns the existing URI).
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

from secantus import SecantusDBServer

DEFAULT_EMBEDDED_STORAGE = Path.home() / ".secantus" / "embedded-data"


class EmbeddedServer:
    """Wraps an optional in-process ``SecantusDBServer`` for the dashboard."""

    def __init__(self, default_storage_path: Path | str | None = None) -> None:
        self.default_storage_path = Path(default_storage_path or DEFAULT_EMBEDDED_STORAGE)
        self._server: SecantusDBServer | None = None
        self._storage_path: Path | None = None
        self._lock = threading.Lock()

    def status(self) -> dict[str, Any]:
        with self._lock:
            if self._server is None:
                return {
                    "running": False,
                    "uri": None,
                    "storage_path": str(self.default_storage_path),
                }
            return {
                "running": True,
                "uri": self._server.uri,
                "storage_path": str(self._storage_path) if self._storage_path else None,
            }

    def start(self, *, storage_path: Path | str | None = None) -> str:
        """Start the embedded server. Idempotent — returns the existing URI
        if one is already running."""
        with self._lock:
            if self._server is not None:
                return self._server.uri
            path = Path(storage_path or self.default_storage_path)
            path.mkdir(parents=True, exist_ok=True)
            srv = SecantusDBServer(port=0, storage_path=str(path))
            srv.start()
            self._server = srv
            self._storage_path = path
            return srv.uri

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


__all__ = ["EmbeddedServer", "DEFAULT_EMBEDDED_STORAGE"]
