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
* ``GET /roles[?db=...]`` — built-in roles + their actions, plus any
  custom roles the target reports, with a create form
* ``POST /roles[?db=...]`` — createRole
* ``POST /roles/{db}/{role}/drop`` — dropRole
"""

from __future__ import annotations

import contextlib
from typing import Any
from urllib.parse import quote

from bson import json_util
from fastapi import APIRouter, Form, HTTPException, Request
from fastapi.responses import HTMLResponse, RedirectResponse
from fastapi.templating import Jinja2Templates

from secantus.admin import capabilities
from secantus.admin.client import MongoError
from secantus.rbac import BUILT_IN_ROLES

router = APIRouter()


def _templates(request: Request) -> Jinja2Templates:
    return Jinja2Templates(directory=request.app.state.templates_dir)


def _all_role_names(request: Request | None = None, db: str = "admin") -> list[str]:
    """Role names to offer in the picker.

    Sourced from the *connected target* (``rolesInfo``) rather than this
    package's own table: the admin UI can point at the Rust server or a
    real ``mongod``, either of which may recognise roles the Python
    server's ``BUILT_IN_ROLES`` doesn't list — most obviously custom
    roles created with ``createRole``. The built-in names are unioned in
    as a floor so a target that can't answer ``rolesInfo`` still renders
    a usable picker instead of an empty one.
    """
    names = set(BUILT_IN_ROLES.keys())
    if request is not None:
        # Target can't answer rolesInfo — the built-in floor stands.
        with contextlib.suppress(MongoError):
            names.update(request.app.state.mongo.list_role_names(db))
    return sorted(names)


def _normalise_roles(payload: list[str] | None, default_db: str) -> list[dict[str, str]]:
    """Coerce form-encoded ``role@db`` strings into mongod's role dicts.

    The form sends each binding as a string of the form ``"<role>@<db>"``.
    Empty strings are dropped so unchecked rows don't sneak through.

    Role *names* are passed through without being checked against this
    package's ``BUILT_IN_ROLES``. The target server is the authority on
    which roles exist — it may be a ``mongod``, or any server with custom
    roles — and filtering here against a local table silently discarded
    valid bindings, leaving the user staring at a role that refused to
    stick with no error. An unknown role now reaches the server and comes
    back as an honest ``RoleNotFound``.
    """
    out: list[dict[str, str]] = []
    for entry in payload or []:
        if not entry:
            continue
        role, _, db = entry.partition("@")
        if not role or not db:
            role = entry
            db = default_db
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
            "all_roles": _all_role_names(request, db),
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
    for role in _all_role_names(request, db):
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
        if capabilities.is_command_not_found(exc):
            capabilities.record_unsupported(request.app, "grant_revoke_roles")
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
    for name in _all_role_names(request):
        spec = BUILT_IN_ROLES.get(name)
        if spec is None:
            # A role the target recognises but this package has no action
            # table for — a custom role created with ``createRole``. Show
            # it so the catalogue matches the target, but don't invent
            # privileges we haven't read.
            rows.append({"name": name, "actions": [], "flags": ["custom"]})
            continue
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
        {
            "title": "Roles",
            "active": "roles",
            "rows": rows,
            "db_name": request.query_params.get("db") or "admin",
            "notice": request.query_params.get("notice") or None,
            "error": None,
        },
    )


def _roles_page_with_error(request: Request, db: str, message: str) -> HTMLResponse:
    """Re-render /roles carrying an error, status 400."""
    rows: list[dict[str, Any]] = []
    for name in _all_role_names(request, db):
        spec = BUILT_IN_ROLES.get(name)
        rows.append(
            {
                "name": name,
                "actions": sorted(spec.actions) if spec else [],
                "flags": [] if spec else ["custom"],
            }
        )
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "pages/roles.html",
        {
            "title": "Roles",
            "active": "roles",
            "rows": rows,
            "db_name": db,
            "notice": None,
            "error": message,
        },
        status_code=400,
    )


def _parse_json_array(raw: str, *, field: str) -> list[Any]:
    """Parse an Extended-JSON array. Empty text means an empty array."""
    if not raw or not raw.strip():
        return []
    try:
        parsed = json_util.loads(raw)
    except (ValueError, TypeError) as exc:
        raise MongoError(f"{field} is not valid Extended JSON: {exc}") from exc
    if not isinstance(parsed, list):
        raise MongoError(f"{field} must be a JSON array.")
    return parsed


@router.post("/roles", response_class=HTMLResponse)
async def create_role(request: Request) -> HTMLResponse:
    form = await request.form()
    db = request.query_params.get("db") or "admin"
    name = str(form.get("name") or "").strip()
    if not name:
        return _roles_page_with_error(request, db, "Role name is required.")
    try:
        privileges = _parse_json_array(str(form.get("privileges") or ""), field="Privileges")
        roles = _parse_json_array(str(form.get("roles") or ""), field="Inherited roles")
        if not privileges and not roles:
            raise MongoError(
                "A role needs at least one privilege or one inherited role — "
                "createRole with both empty grants nothing."
            )
        request.app.state.mongo.create_role(db, name, privileges=privileges, roles=roles)
    except MongoError as exc:
        return _roles_page_with_error(request, db, str(exc))
    return RedirectResponse(
        f"/roles?db={quote(db)}&notice={quote(f'Created role {name}')}", status_code=303
    )


@router.post("/roles/{db}/{role}/drop", response_class=HTMLResponse)
def drop_role(request: Request, db: str, role: str) -> HTMLResponse:
    try:
        request.app.state.mongo.drop_role(db, role)
    except MongoError as exc:
        return _roles_page_with_error(request, db, str(exc))
    return RedirectResponse(
        f"/roles?db={quote(db)}&notice={quote(f'Dropped role {role}')}", status_code=303
    )
