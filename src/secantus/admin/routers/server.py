"""Target-server management page.

Distinct from ``/connections`` (plural — the list of active TCP
connections to the current target). This page lets the operator
switch which target the admin UI talks to, without restarting the
process.

* ``GET /server`` — current URI, switch form, list of recently-used
  URIs as quick-switch buttons.
* ``POST /server/switch`` — issues a target swap; redirects back
  on success, re-renders with an error on failure.
* ``POST /server/forget`` — drops a saved URI from the list.
"""

from __future__ import annotations

from fastapi import APIRouter, Form, Request
from fastapi.responses import HTMLResponse
from fastapi.templating import Jinja2Templates

from secantus.admin import embedded as embedded_lib
from secantus.admin.client import display_uri
from secantus.admin.swap import SwapError, swap_target

router = APIRouter()


def _templates(request: Request) -> Jinja2Templates:
    return Jinja2Templates(directory=request.app.state.templates_dir)


def _render(
    request: Request,
    *,
    error: str | None = None,
    flash: str | None = None,
    pending_uri: str = "",
    status_code: int = 200,
    embedded_flash: dict[str, str] | None = None,
    embedded_error: str | None = None,
    pending_storage: str = "",
) -> HTMLResponse:
    targets = []
    try:
        for entry in request.app.state.targets.recent():
            targets.append(
                {
                    "uri": entry.uri,
                    "display_uri": display_uri(entry.uri),
                    "is_current": entry.uri == request.app.state.mongo_uri,
                    "last_used_at": entry.last_used_at,
                }
            )
    except Exception:
        targets = []
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "pages/server.html",
        {
            "title": "Server",
            "active": "server",
            "current_uri_display": request.app.state.mongo_uri_display,
            "targets": targets,
            "error": error,
            "flash": flash,
            "pending_uri": pending_uri,
            "embedded": request.app.state.embedded.status(),
            "embedded_default_path": str(request.app.state.embedded.default_storage_path),
            "embedded_flavours": embedded_lib.available_flavours(),
            "embedded_flash": embedded_flash,
            "embedded_error": embedded_error,
            "pending_storage": pending_storage,
        },
        status_code=status_code,
    )


@router.get("/server", response_class=HTMLResponse)
def server_page(request: Request) -> HTMLResponse:
    return _render(request)


@router.post("/server/switch", response_class=HTMLResponse)
def post_switch(
    request: Request,
    uri: str = Form(...),
) -> HTMLResponse:
    new_uri = uri.strip()
    if not new_uri:
        return _render(request, error="URI is required", status_code=400)
    if new_uri == request.app.state.mongo_uri:
        return _render(
            request,
            flash="Already connected to that URI.",
            pending_uri="",
        )
    try:
        swap_target(request.app, new_uri)
    except SwapError as exc:
        return _render(request, error=str(exc), pending_uri=new_uri, status_code=400)
    return _render(request, flash=f"Switched to {display_uri(new_uri)}")


@router.post("/server/forget", response_class=HTMLResponse)
def post_forget(
    request: Request,
    uri: str = Form(...),
) -> HTMLResponse:
    if not uri.strip():
        return _render(request, error="URI is required", status_code=400)
    if uri == request.app.state.mongo_uri:
        return _render(
            request,
            error="Can't forget the URI you're currently connected to.",
            status_code=400,
        )
    request.app.state.targets.forget(uri)
    return _render(request, flash=f"Forgot {display_uri(uri)}")


@router.post("/embedded/start", response_class=HTMLResponse)
def post_embedded_start(
    request: Request,
    storage_path: str = Form(""),
    flavour: str = Form(embedded_lib.DEFAULT_FLAVOUR),
) -> HTMLResponse:
    path = storage_path.strip() or None
    try:
        uri = request.app.state.embedded.start(storage_path=path, flavour=flavour)
    except (OSError, ValueError, RuntimeError) as exc:
        # ValueError = unknown flavour, RuntimeError = Rust extension not
        # installed. Both are user-fixable, so surface the message rather
        # than a 500.
        return _render(
            request,
            embedded_error=f"Could not start embedded server: {exc}",
            pending_storage=storage_path,
        )
    try:
        swap_target(request.app, uri)
        flash = {"kind": "ok", "msg": f"Started embedded server at {uri}"}
    except SwapError as exc:
        flash = {
            "kind": "err",
            "msg": (
                f"Started embedded server at {uri}, but couldn't switch the admin app to it: {exc}"
            ),
        }
    return _render(request, embedded_flash=flash)


@router.post("/embedded/stop", response_class=HTMLResponse)
def post_embedded_stop(request: Request) -> HTMLResponse:
    request.app.state.embedded.stop()
    return _render(
        request,
        embedded_flash={"kind": "ok", "msg": "Stopped embedded server."},
    )
