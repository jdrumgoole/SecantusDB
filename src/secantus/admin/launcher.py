"""Process lifecycle for ``secantus-admin``.

Boots uvicorn in a daemon thread, waits for ``/healthz`` to come up,
then either opens a pywebview window (default) or blocks on the server
(``--no-window`` / CI). Closing the window triggers a clean uvicorn
shutdown.
"""

from __future__ import annotations

import logging
import threading
import time
import urllib.request

import uvicorn

from secantus.admin.app import create_app

logger = logging.getLogger(__name__)


def _wait_for_ready(host: str, port: int, *, timeout: float = 10.0) -> bool:
    """Poll ``/healthz`` until it returns 200 or the timeout elapses."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(f"http://{host}:{port}/healthz", timeout=0.5) as r:
                if r.status == 200:
                    return True
        except Exception:
            pass
        time.sleep(0.05)
    return False


def run(*, mongo_uri: str, port: int, token: str, no_window: bool) -> int:
    """Launch the admin app. Returns a process exit code."""
    app = create_app(mongo_uri=mongo_uri, token=token)
    config = uvicorn.Config(
        app,
        host="127.0.0.1",
        port=port,
        log_level="warning",
        access_log=False,
    )
    server = uvicorn.Server(config)

    thread = threading.Thread(target=server.run, name="secantus-admin-uvicorn", daemon=True)
    thread.start()

    # Bind happens inside ``server.run``; spin until the chosen port is
    # known and the app responds. uvicorn assigns ``server.servers``
    # once the asyncio loop is up.
    actual_port = port
    deadline = time.monotonic() + 5.0
    while time.monotonic() < deadline and not server.started:
        time.sleep(0.02)
    if server.started and server.servers:
        sockets = server.servers[0].sockets or []
        if sockets:
            actual_port = sockets[0].getsockname()[1]

    if not _wait_for_ready("127.0.0.1", actual_port):
        logger.error("admin app failed to start on 127.0.0.1:%s", actual_port)
        server.should_exit = True
        thread.join(timeout=2.0)
        return 1

    url = f"http://127.0.0.1:{actual_port}/?t={token}"
    logger.info("admin app ready at %s", url)

    if no_window:
        try:
            thread.join()
        except KeyboardInterrupt:
            pass
        finally:
            server.should_exit = True
        return 0

    # pywebview is imported lazily so test runs that go through
    # ``--no-window`` don't pay the GUI dependency cost.
    import webview

    webview.create_window("SecantusDB admin", url, width=1280, height=900)
    try:
        webview.start()
    finally:
        server.should_exit = True
        thread.join(timeout=2.0)
    return 0


__all__ = ["run"]
