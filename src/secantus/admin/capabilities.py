"""Detect what the connected target server can do, so the admin UI can
gate features it would otherwise advertise but the server can't honour.

The admin UI is a plain pymongo client and can point at any MongoDB-wire
server: the SecantusDB Python server, the SecantusDB Rust server, or a
real ``mongod``. They differ in which commands they implement — most
visibly the proprietary ``secantusAdmin.*`` maintenance / backup / PITR
commands, which no ``mongod`` has. Rather than let the user click a
button that returns ``CommandNotFound``, the app probes the target once
(``buildInfo`` + ``serverStatus``) and derives a capability set the
templates consult.

Detection keys off the server's own self-identification:

* ``serverStatus.secantus.server`` is ``"python"`` / ``"rust"`` on the
  two SecantusDB servers;
* ``buildInfo.secantusVersion`` is present on both (absent on ``mongod``).

**Why there is no per-SecantusDB-flavour feature table.** There used to
be one, listing what the Rust server had not yet ported. It went stale
almost immediately — the Rust server grew ``pruneOplog`` / ``pruneTtl``
/ ``restoreArchive`` / ``killOp`` / real ``getLog`` / real ``profile``
within days, and the table kept claiming otherwise for months, so the
UI hid six working feature groups behind disabled buttons. That is the
exact inverse of the failure this module exists to prevent. Any
hardcoded "server X can't do Y yet" list is a snapshot of a moving
target and will drift again, so we no longer keep one:

* **SecantusDB targets (python / rust) start fully permissive.** Both
  servers track each other's command surface closely; assuming parity
  and being occasionally wrong costs one honest ``CommandNotFound``,
  whereas assuming a gap that has since closed silently removes working
  functionality with no signal to anyone.
* **A flag is cleared only on evidence.** When a gated command actually
  comes back ``CommandNotFound`` (code 59), the router calls
  :func:`record_unsupported` and the button disappears for the rest of
  the session. Negative knowledge is *learned from the live server*,
  never hardcoded.
* **``mongod`` keeps a static profile** because its negatives are
  definitional, not a porting snapshot: no ``mongod`` will ever
  implement the proprietary ``secantusAdmin.*`` commands, and every
  standard admin command it does implement is stable.

An unreachable / not-yet-probed target yields the permissive
:data:`UNKNOWN` set — nothing is hidden until we positively know the
server can't do it, so a transiently-down target never hides a working
button.
"""

from __future__ import annotations

import dataclasses
from dataclasses import dataclass
from typing import Any

from secantus.admin.client import MongoError

# MongoDB's CommandNotFound. The only signal we accept as proof that a
# target genuinely lacks a command.
COMMAND_NOT_FOUND = 59

# Feature flags consulted by the templates. Each maps to one or more
# wire commands the corresponding admin button issues.
_FLAGS = (
    "native_backup_archive",  # secantusAdmin.backupArchive
    "native_restore_archive",  # secantusAdmin.restoreArchive
    "native_prune",  # secantusAdmin.pruneOplog + .pruneTtl
    "native_pitr",  # secantusAdmin.archiveBaseSnapshot + .restoreToTimestamp
    "grant_revoke_roles",  # grantRolesToUser / revokeRolesFromUser
    "kill_op",  # killOp
    "server_log",  # getLog
    "profiling",  # profile
)

# The proprietary commands — the ones a real ``mongod`` definitionally
# cannot serve. Everything else in _FLAGS is a standard MongoDB command.
_PROPRIETARY_FLAGS = frozenset(
    {
        "native_backup_archive",
        "native_restore_archive",
        "native_prune",
        "native_pitr",
    }
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
    native_pitr: bool
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

    def without(self, flag: str) -> ServerCapabilities:
        """Return a copy with ``flag`` cleared.

        Used by :func:`record_unsupported` after a live ``CommandNotFound``.
        """
        if flag not in _FLAGS:
            raise ValueError(f"unknown capability flag: {flag!r}")
        return dataclasses.replace(self, **{flag: False})


# Permissive default: before a successful probe we show everything, so a
# transiently-unreachable server never hides working buttons.
UNKNOWN = ServerCapabilities(
    kind="unknown",
    label="Not yet identified",
    version="",
    **dict.fromkeys(_FLAGS, True),
)

# A real ``mongod``: every standard admin command, none of the
# proprietary ones. Unlike a per-flavour porting table, these negatives
# never go stale.
_MONGODB_FLAGS = {flag: flag not in _PROPRIETARY_FLAGS for flag in _FLAGS}


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
        # from serverStatus — still a SecantusDB, so still permissive.
        kind = "python"
    else:
        kind = "mongodb"

    if kind == "mongodb":
        flags = dict(_MONGODB_FLAGS)
        version = str((build_info or {}).get("version") or "")
        label = f"MongoDB {version}".rstrip()
    else:
        # Both SecantusDB servers start fully permissive; anything they
        # genuinely lack is learned from a live CommandNotFound.
        flags = dict.fromkeys(_FLAGS, True)
        version = str(secantus_version or secantus_meta.get("version") or "")
        flavour = "Rust" if kind == "rust" else "Python"
        label = f"SecantusDB ({flavour})"
        if version:
            label = f"{label} {version}"

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


def record_unsupported(app: Any, flag: str) -> None:
    """Clear ``flag`` on the app's capability set after a live failure.

    Call from a router when a gated command comes back
    ``CommandNotFound``: the target has told us, authoritatively, that
    it does not implement that command, so the button should stop being
    offered for the rest of the session. A target swap re-probes and
    resets to the permissive default.
    """
    current = getattr(app.state, "capabilities", None)
    if current is None:
        return
    if not getattr(current, flag, False):
        return  # already cleared
    app.state.capabilities = current.without(flag)


def is_command_not_found(exc: MongoError) -> bool:
    """True when a ``MongoError`` is the server saying "I don't have that"."""
    return getattr(exc, "code", None) == COMMAND_NOT_FOUND


__all__ = [
    "ServerCapabilities",
    "UNKNOWN",
    "COMMAND_NOT_FOUND",
    "classify",
    "probe",
    "record_unsupported",
    "is_command_not_found",
]
