"""Users + roles UI.

The user-management surface mirrors mongod's: users live on a
"home database" (typically ``admin``) and carry role bindings that
point at any database. ``?db=`` switches the currently-shown home db;
defaults to ``admin``.

Endpoints:

* ``GET /users[?db=...]`` — list + create form
* ``POST /users[?db=...]`` — createUser
* ``GET /users/{db}/{user}/password`` — change-password modal
* ``POST /users/{db}/{user}/password`` — updateUser
* ``GET /users/{db}/{user}/roles`` — manage-roles modal
* ``POST /users/{db}/{user}/roles`` — diff against current and emit
  ``grantRolesToUser`` / ``revokeRolesFromUser`` as needed
* ``GET /users/{db}/{user}/drop-confirm`` — drop modal
* ``DELETE /users/{db}/{user}`` — dropUser
* ``GET /roles`` — read-only built-in roles + their actions
"""

from __future__ import annotations

from typing import Any

from fastapi import APIRouter, Form, HTTPException, Request
from fastapi.responses import HTMLResponse
from fastapi.templating import Jinja2Templates

from secantus.admin.client import MongoError
from secantus.rbac import BUILT_IN_ROLES

router = APIRouter()


def _templates(request: Request) -> Jinja2Templates:
    return Jinja2Templates(directory=request.app.state.templates_dir)


def _all_role_names() -> list[str]:
    return sorted(BUILT_IN_ROLES.keys())


def _normalise_roles(payload: list[str] | None, default_db: str) -> list[dict[str, str]]:
    """Coerce form-encoded ``role@db`` strings into mongod's role dicts.

    The form sends each binding as a string of the form ``"<role>@<db>"``.
    Empty strings are dropped so unchecked rows don't sneak through.
    """
    out: list[dict[str, str]] = []
    for entry in payload or []:
        if not entry:
            continue
        role, _, db = entry.partition("@")
        if not role or not db:
            role = entry
            db = default_db
        if role in BUILT_IN_ROLES:
            out.append({"role": role, "db": db})
    return out


def _user_role_set(user: dict[str, Any]) -> set[str]:
    """Set of ``role@db`` strings currently bound to ``user``."""
    return {f"{r['role']}@{r['db']}" for r in user.get("roles") or [] if isinstance(r, dict)}


# ---- /users -----------------------------------------------------------------


@router.get("/users", response_class=HTMLResponse)
def users_page(request: Request) -> HTMLResponse:
    db = request.query_params.get("db") or "admin"
    mongo = request.app.state.mongo
    error: str | None = None
    users: list[dict[str, Any]] = []
    try:
        users = mongo.list_users(db)
    except MongoError as exc:
        error = str(exc)
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "pages/users.html",
        {
            "title": "Users",
            "active": "users",
            "db_name": db,
            "users": users,
            "all_roles": _all_role_names(),
            "error": error,
        },
    )


@router.post("/users", response_class=HTMLResponse)
async def create_user(request: Request) -> HTMLResponse:
    form = await request.form()
    db = request.query_params.get("db") or "admin"
    username = (form.get("username") or "").strip()
    password = form.get("password") or ""
    raw_roles = form.getlist("roles")
    if not username or not password:
        raise HTTPException(status_code=400, detail="username and password are required")
    roles = _normalise_roles(list(raw_roles), default_db=db)
    if not roles:
        raise HTTPException(status_code=400, detail="at least one valid role is required")
    try:
        request.app.state.mongo.create_user(db, username, password, roles)
    except MongoError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    return HTMLResponse(
        "",
        status_code=303,
        headers={
            "HX-Redirect": f"/users?db={db}",
            "Location": f"/users?db={db}",
        },
    )


# ---- per-user actions -------------------------------------------------------


@router.get("/users/{db}/{username}/password", response_class=HTMLResponse)
def password_modal(request: Request, db: str, username: str) -> HTMLResponse:
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "partials/user_password_modal.html",
        {"db_name": db, "username": username, "error": None},
    )


@router.post("/users/{db}/{username}/password", response_class=HTMLResponse)
def change_password(
    request: Request,
    db: str,
    username: str,
    password: str = Form(...),
    confirm: str = Form(...),
) -> HTMLResponse:
    templates = _templates(request)

    def _modal(error: str) -> HTMLResponse:
        return templates.TemplateResponse(
            request,
            "partials/user_password_modal.html",
            {"db_name": db, "username": username, "error": error},
            status_code=400,
        )

    if not password:
        return _modal("Password is required.")
    if password != confirm:
        return _modal("Passwords do not match.")
    try:
        request.app.state.mongo.update_user_password(db, username, password)
    except MongoError as exc:
        return _modal(str(exc))
    return HTMLResponse(
        "",
        headers={"HX-Trigger": "password-updated"},
    )


@router.get("/users/{db}/{username}/roles", response_class=HTMLResponse)
def roles_modal(request: Request, db: str, username: str) -> HTMLResponse:
    try:
        user = request.app.state.mongo.get_user(db, username)
    except MongoError as exc:
        raise HTTPException(status_code=502, detail=str(exc)) from exc
    if user is None:
        raise HTTPException(status_code=404, detail="user not found")
    current = _user_role_set(user)
    targets: set[str] = set()
    for role in _all_role_names():
        # In mongod, *AnyDatabase roles must bind to admin; the rest can
        # bind to any database. Surface a candidate per-db option that
        # makes sense for that role.
        targets.add(f"{role}@admin")
        targets.add(f"{role}@{db}")
    candidates = sorted(targets)
    rows = [{"value": c, "checked": c in current} for c in candidates]
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "partials/user_roles_modal.html",
        {
            "db_name": db,
            "username": username,
            "rows": rows,
            "current_count": len(current),
        },
    )


@router.post("/users/{db}/{username}/roles", response_class=HTMLResponse)
async def update_roles(request: Request, db: str, username: str) -> HTMLResponse:
    form = await request.form()
    desired_pairs = _normalise_roles(list(form.getlist("roles")), default_db=db)
    desired = {f"{r['role']}@{r['db']}" for r in desired_pairs}

    try:
        user = request.app.state.mongo.get_user(db, username)
    except MongoError as exc:
        raise HTTPException(status_code=502, detail=str(exc)) from exc
    if user is None:
        raise HTTPException(status_code=404, detail="user not found")
    current = _user_role_set(user)

    to_grant_pairs = [r for r in desired_pairs if f"{r['role']}@{r['db']}" not in current]
    to_revoke = current - desired
    to_revoke_pairs = []
    for s in to_revoke:
        role, _, role_db = s.partition("@")
        to_revoke_pairs.append({"role": role, "db": role_db})

    try:
        if to_grant_pairs:
            request.app.state.mongo.grant_roles(db, username, to_grant_pairs)
        if to_revoke_pairs:
            request.app.state.mongo.revoke_roles(db, username, to_revoke_pairs)
    except MongoError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    return HTMLResponse(
        "",
        headers={"HX-Trigger": "roles-updated", "HX-Redirect": f"/users?db={db}"},
    )


@router.get("/users/{db}/{username}/drop-confirm", response_class=HTMLResponse)
def drop_user_modal(request: Request, db: str, username: str) -> HTMLResponse:
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "partials/user_drop_modal.html",
        {"db_name": db, "username": username},
    )


@router.delete("/users/{db}/{username}", response_class=HTMLResponse)
def drop_user(request: Request, db: str, username: str) -> HTMLResponse:
    try:
        request.app.state.mongo.drop_user(db, username)
    except MongoError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    return HTMLResponse("", headers={"HX-Trigger": "user-dropped"})


# ---- /roles -----------------------------------------------------------------


@router.get("/roles", response_class=HTMLResponse)
def roles_page(request: Request) -> HTMLResponse:
    rows: list[dict[str, Any]] = []
    for name in _all_role_names():
        spec = BUILT_IN_ROLES[name]
        flags: list[str] = []
        if getattr(spec, "any_db", False):
            flags.append("any_db")
        if getattr(spec, "cluster", False):
            flags.append("cluster")
        if getattr(spec, "admin_only", False):
            flags.append("admin_only")
        rows.append(
            {
                "name": name,
                "actions": sorted(spec.actions),
                "flags": flags,
            }
        )
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "pages/roles.html",
        {"title": "Roles", "active": "roles", "rows": rows},
    )
