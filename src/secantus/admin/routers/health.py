"""Unauthenticated ``/healthz`` endpoint.

Used by the launcher's readiness probe and by anyone monitoring the
admin process. The probe also pings the target SecantusDB so a healthy
``200`` means "this admin app can talk to its target", not just "this
HTTP server is up".
"""

from __future__ import annotations

from fastapi import APIRouter, Request

import secantus

router = APIRouter()


@router.get("/healthz")
def healthz(request: Request) -> dict[str, object]:
    mongo = request.app.state.mongo
    health = mongo.ping()
    return {
        "ok": True,
        "version": secantus.__version__,
        "mongo_ok": health.ok,
        "mongo_detail": health.detail,
    }
