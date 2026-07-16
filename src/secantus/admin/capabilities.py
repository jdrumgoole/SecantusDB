"""Detect what the connected target server can do, so the admin UI can
gate features it would otherwise advertise but the server can't honour.

The admin UI is a plain pymongo client and can point at any MongoDB-wire
server: the SecantusDB Python server, the SecantusDB Rust server, or a
real ``mongod``. They differ in which commands they implement — most
visibly the four proprietary ``secantusAdmin.*`` maintenance / backup
commands (no ``mongod`` has them) and a handful of standard admin
commands the Rust server hasn't ported yet. Rather than let the user
click a button that returns ``CommandNotFound``, the app probes the
target once (``buildInfo`` + ``serverStatus``) and derives a capability
set the templates consult.

Detection keys off the server's own self-identification:

* ``serverStatus.secantus.server`` is ``"python"`` / ``"rust"`` on the
  two SecantusDB servers;
* ``buildInfo.secantusVersion`` is present on both (absent on ``mongod``).

An unreachable / not-yet-probed target yields the permissive
:data:`UNKNOWN` set — nothing is hidden until we positively know the
server can't do it, so a transiently-down target never hides a working
button.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from secantus.admin.client import MongoError

# Feature flags consulted by the templates. Each maps to one or more
# wire commands the corresponding admin button issues.
_FLAGS = (
    "native_backup_archive",  # secantusAdmin.backupArchive
    "native_restore_archive",  # secantusAdmin.restoreArchive
    "native_prune",  # secantusAdmin.pruneOplog + .pruneTtl
    "grant_revoke_roles",  # grantRolesToUser / revokeRolesFromUser
    "kill_op",  # killOp
    "server_log",  # getLog (real ring buffer, not an empty stub)
    "profiling",  # profile (real slow-op capture, not level-only)
)


@dataclass(frozen=True)
class ServerCapabilities:
    """What the current target server supports, for UI gating.

    ``kind`` is ``"python"`` / ``"rust"`` / ``"mongodb"`` / ``"unknown"``.
    The boolean flags mirror :data:`_FLAGS`.
    """

    kind: str
    label: str
    version: str
    native_backup_archive: bool
    native_restore_archive: bool
    native_prune: bool
    grant_revoke_roles: bool
    kill_op: bool
    server_log: bool
    profiling: bool

    @property
    def is_secantus(self) -> bool:
        return self.kind in ("python", "rust")

    @property
    def identified(self) -> bool:
        return self.kind != "unknown"


# Permissive default: before a successful probe we show everything, so a
# transiently-unreachable server never hides working buttons.
UNKNOWN = ServerCapabilities(
    kind="unknown",
    label="Not yet identified",
    version="",
    **dict.fromkeys(_FLAGS, True),
)

# Per-server-kind capability profile. Keyed to the command inventories in
# the Python server (``src/secantus/commands.py``) and the Rust server
# (``crates/secantus-commands``): the Rust server has not yet ported
# ``restoreArchive`` / ``pruneOplog`` / ``pruneTtl`` /
# ``grantRolesToUser`` / ``revokeRolesFromUser`` / ``killOp``, and its
# ``getLog`` + ``profile`` are hollow stubs. A real ``mongod`` has every
# standard command but none of the proprietary ``secantusAdmin.*`` ones.
_PROFILE: dict[str, dict[str, bool]] = {
    "python": dict.fromkeys(_FLAGS, True),
    "rust": {
        "native_backup_archive": True,
        "native_restore_archive": False,
        "native_prune": False,
        "grant_revoke_roles": False,
        "kill_op": False,
        "server_log": False,
        "profiling": False,
    },
    "mongodb": {
        "native_backup_archive": False,
        "native_restore_archive": False,
        "native_prune": False,
        "grant_revoke_roles": True,
        "kill_op": True,
        "server_log": True,
        "profiling": True,
    },
}


def classify(
    build_info: dict[str, Any] | None,
    server_status: dict[str, Any] | None,
) -> ServerCapabilities:
    """Derive a capability set from a target's ``buildInfo`` + ``serverStatus``."""
    secantus_meta = (server_status or {}).get("secantus") or {}
    server = secantus_meta.get("server")
    secantus_version = (build_info or {}).get("secantusVersion")

    if server in ("python", "rust"):
        kind = server
    elif secantus_version:
        # SecantusDB, but we couldn't read the python/rust discriminator
        # from serverStatus — be permissive (treat as the full
        # Python-level surface) rather than wrongly hide a working
        # command.
        kind = "python"
    else:
        kind = "mongodb"

    flags = _PROFILE[kind]
    if kind in ("python", "rust"):
        version = str(secantus_version or secantus_meta.get("version") or "")
        flavour = "Rust" if kind == "rust" else "Python"
        label = f"SecantusDB ({flavour})"
        if version:
            label = f"{label} {version}"
    else:
        version = str((build_info or {}).get("version") or "")
        label = f"MongoDB {version}".rstrip()

    return ServerCapabilities(kind=kind, label=label, version=version, **flags)


def probe(facade: Any) -> ServerCapabilities:
    """Probe a live ``MongoFacade`` and classify it.

    Reads ``buildInfo`` + ``serverStatus`` (either may fail
    independently — e.g. an auth-restricted ``serverStatus``). Raises
    :class:`MongoError` only when *both* probes fail, so the caller can
    fall back to :data:`UNKNOWN` and retry later.
    """
    build_info: dict[str, Any] = {}
    server_status: dict[str, Any] = {}
    try:
        build_info = facade.build_info()
    except MongoError:
        build_info = {}
    try:
        server_status = facade.server_status()
    except MongoError:
        server_status = {}
    if not build_info and not server_status:
        raise MongoError("could not probe server capabilities")
    return classify(build_info, server_status)


__all__ = ["ServerCapabilities", "UNKNOWN", "classify", "probe"]
