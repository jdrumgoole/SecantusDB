"""SecantusDB Ops Board — a local web app to drive and observe the
build / test / release cycle for all three SecantusDB servers.

Optional; needs the ``opsboard`` extra (fastapi / uvicorn / pywebview). The
job-tracking core lives in ``secantus.jobkit`` and is shared with the ``./inv``
CLI so terminal- and UI-started builds are one journaled process.
"""

from __future__ import annotations

__all__ = ["create_app"]


def __getattr__(name: str):  # noqa: ANN202
    # Lazy so ``import secantus.opsboard`` doesn't require the web extra unless
    # the app is actually constructed.
    if name == "create_app":
        from secantus.opsboard.app import create_app

        return create_app
    raise AttributeError(name)
