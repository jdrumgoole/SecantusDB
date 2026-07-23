"""Process lifecycle for ``secantus-opsboard``.

Boots uvicorn in a daemon thread (the web server MUST NOT be on the main
thread — pywebview owns that on macOS), waits for ``/healthz``, then opens a
pywebview window (default) or blocks headless (``--no-window`` / CI).
"""

from __future__ import annotations

import logging
import threading
import time
import urllib.request
from pathlib import Path

import uvicorn

from secantus.opsboard.app import create_app

logger = logging.getLogger(__name__)


def _wait_for_ready(host: str, port: int, *, timeout: float = 10.0) -> bool:
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


def run(
    *,
    repo_root: str | Path | None,
    port: int,
    token: str,
    no_window: bool,
    host: str = "127.0.0.1",
    journal_path: str | Path | None = None,
) -> int:
    app = create_app(repo_root=repo_root, token=token, journal_path=journal_path)
    config = uvicorn.Config(app, host=host, port=port, log_level="warning", access_log=False)
    server = uvicorn.Server(config)

    thread = threading.Thread(target=server.run, name="secantus-opsboard-uvicorn", daemon=True)
    thread.start()

    actual_port = port
    deadline = time.monotonic() + 5.0
    while time.monotonic() < deadline and not server.started:
        time.sleep(0.02)
    if server.started and server.servers:
        sockets = server.servers[0].sockets or []
        if sockets:
            actual_port = sockets[0].getsockname()[1]

    probe_host = "127.0.0.1" if host in ("0.0.0.0", "") else host
    if not _wait_for_ready(probe_host, actual_port):
        logger.error("opsboard failed to start on %s:%s", probe_host, actual_port)
        server.should_exit = True
        thread.join(timeout=2.0)
        return 1

    url = f"http://{probe_host}:{actual_port}/?t={token}"
    logger.info("opsboard ready at %s", url)

    if no_window:
        try:
            thread.join()
        except KeyboardInterrupt:
            pass
        finally:
            server.should_exit = True
        return 0

    import webview  # lazy: the --no-window path never pays the GUI cost

    webview.create_window("SecantusDB Ops Board", url, width=1280, height=900)
    try:
        webview.start()
    finally:
        server.should_exit = True
        thread.join(timeout=2.0)
    return 0


__all__ = ["run"]
