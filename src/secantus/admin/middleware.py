"""Token-gate middleware for the admin app.

Loopback-only deployments still benefit from a token: a malicious
process running as the same user could otherwise reach the admin port
on ``127.0.0.1`` and impersonate the operator. The token mitigates
same-host CSRF and accidental browser tab access.

Acceptance order per request (first match wins):

1. ``?t=<token>`` query string — used on the initial pywebview load.
2. ``X-Admin-Token`` header — used by HTMX / JS calls after first load.
3. ``secantus-admin-token`` cookie — set on first successful auth so
   subsequent navigation works without the URL parameter.

``/healthz`` and ``/static/*`` skip the check; they leak no state.
"""

from __future__ import annotations

from collections.abc import Awaitable, Callable

from starlette.middleware.base import BaseHTTPMiddleware
from starlette.requests import Request
from starlette.responses import JSONResponse, Response

COOKIE_NAME = "secantus-admin-token"
HEADER_NAME = "X-Admin-Token"
QUERY_NAME = "t"

_BYPASS_PREFIXES = ("/healthz", "/static/")


class TokenAuthMiddleware(BaseHTTPMiddleware):
    """Reject requests that don't carry the configured token."""

    def __init__(self, app, *, token: str) -> None:
        super().__init__(app)
        if not token:
            raise ValueError("admin token must be a non-empty string")
        self._token = token

    async def dispatch(
        self, request: Request, call_next: Callable[[Request], Awaitable[Response]]
    ) -> Response:
        path = request.url.path
        if any(path == p or path.startswith(p) for p in _BYPASS_PREFIXES):
            return await call_next(request)

        presented = (
            request.query_params.get(QUERY_NAME)
            or request.headers.get(HEADER_NAME)
            or request.cookies.get(COOKIE_NAME)
        )
        if presented != self._token:
            return JSONResponse(
                {"error": "missing or invalid admin token"},
                status_code=401,
            )

        response = await call_next(request)
        # Set the cookie on first valid load so subsequent same-origin
        # requests don't need the query param. Loopback only, HttpOnly
        # so JS can't exfil it, SameSite=Strict to block cross-site
        # links from carrying it.
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
