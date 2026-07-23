"""Token-gate middleware for the Ops Board app.

Loopback-only, but a per-launch token stops another local process (or a stray
browser tab) from reaching the port and triggering builds/releases. Mirrors the
admin app's ``TokenAuthMiddleware`` (same acceptance order and Host-spoof
hardening) with Ops-Board-specific cookie/header names.
"""

from __future__ import annotations

import hmac
from collections.abc import Awaitable, Callable

from starlette.middleware.base import BaseHTTPMiddleware
from starlette.requests import Request
from starlette.responses import JSONResponse, Response

COOKIE_NAME = "secantus-opsboard-token"
HEADER_NAME = "X-Opsboard-Token"
QUERY_NAME = "t"

_BYPASS_PREFIXES = ("/healthz", "/static/")


class TokenAuthMiddleware(BaseHTTPMiddleware):
    def __init__(self, app, *, token: str) -> None:
        super().__init__(app)
        if not token:
            raise ValueError("opsboard token must be a non-empty string")
        self._token = token

    async def dispatch(
        self, request: Request, call_next: Callable[[Request], Awaitable[Response]]
    ) -> Response:
        # scope["path"] (not request.url.path, which is rebuilt from the
        # spoofable Host header — CVE-2026-48710) so a crafted Host can't fake a
        # bypass prefix.
        path = request.scope.get("path", request.url.path)
        if any(path == p or path.startswith(p) for p in _BYPASS_PREFIXES):
            return await call_next(request)

        presented = (
            request.query_params.get(QUERY_NAME)
            or request.headers.get(HEADER_NAME)
            or request.cookies.get(COOKIE_NAME)
        )
        if presented is None or not hmac.compare_digest(
            presented.encode("utf-8"), self._token.encode("utf-8")
        ):
            return JSONResponse({"error": "missing or invalid opsboard token"}, status_code=401)

        response = await call_next(request)
        if request.cookies.get(COOKIE_NAME) != self._token:
            response.set_cookie(
                COOKIE_NAME,
                self._token,
                httponly=True,
                samesite="strict",
                secure=False,
                path="/",
            )
        return response


__all__ = ["TokenAuthMiddleware", "COOKIE_NAME", "HEADER_NAME", "QUERY_NAME"]
