"""TOML-based configuration loader for the ``secantusd-py`` daemon.

The config file is a thin convenience over the CLI flag surface plus
a handful of previously-hard-coded WiredTiger / oplog knobs that
production-shaped deployments want to tune. The CLI itself still
works exactly as before — passing **no** ``--config`` and **no**
``secantusd.toml`` in the auto-discovery path leaves you with the
original behaviour.

Precedence (low → high):

    SecantusConfig defaults  <  TOML file values  <  explicit CLI flag

That's the standard "the more specific wins" model: the file
encodes a per-deployment baseline; a CLI flag overrides for a
one-off run.

Auto-discovery path order (first hit wins):

    1. Explicit ``--config PATH`` if passed (no auto-discovery).
    2. ``./secantusd.toml``                  (cwd — per-checkout)
    3. ``~/.secantus/secantusd.toml``        (per-user)
    4. ``/etc/secantus/secantusd.toml``      (system-wide)

The legacy ``secantusdb.toml`` name is still discovered at each
location (immediately after the new name) so configs written for the
old daemon name keep working; the new ``secantusd.toml`` wins when
both are present.
"""

from __future__ import annotations

from dataclasses import dataclass, fields, replace
from pathlib import Path
from typing import Any

try:
    import tomllib
except ModuleNotFoundError:  # Python 3.10: tomllib is stdlib only from 3.11
    import tomli as tomllib  # type: ignore[no-redef]

# Auto-discovery candidates, in order. The launcher walks this list
# only when ``--config`` was not passed. Each location is probed for
# the new ``secantusd.toml`` first, then the legacy ``secantusdb.toml``.
_CONFIG_NAMES: tuple[str, ...] = ("secantusd.toml", "secantusdb.toml")
_CONFIG_DIRS: tuple[Path, ...] = (
    Path("."),
    Path.home() / ".secantus",
    Path("/etc/secantus"),
)
_AUTO_DISCOVERY_PATHS: tuple[Path, ...] = tuple(
    (d / name if d != Path(".") else Path(name)) for d in _CONFIG_DIRS for name in _CONFIG_NAMES
)


@dataclass(frozen=True)
class SecantusConfig:
    """Resolved configuration for a SecantusDB daemon run.

    Field defaults match the CLI's defaults so a server constructed
    from ``SecantusConfig()`` (no file, no flags) behaves identically
    to what ``secantusd-py`` with zero arguments used to do.
    """

    # ---- [server] ---------------------------------------------------
    host: str = "127.0.0.1"
    port: int = 27017
    storage_path: str = "./secantus-data"
    log_level: str = "INFO"
    auth: bool = False
    # ``False`` means "advertise as single-node replica set" (the
    # SecantusDB default that lets pymongo's change-stream topology
    # checks pass). ``True`` flips back to STANDALONE.
    standalone: bool = False

    # ---- [oplog] ----------------------------------------------------
    oplog_retention_seconds: float = 3600.0
    oplog_max_entries: int = 100_000
    oplog_archive_dir: str | None = None
    noop_heartbeat_seconds: float = 0.0

    # ---- [storage] --------------------------------------------------
    # WT's cache_size config. Accepts unit-suffixed strings: "256M",
    # "1G", "8G". The default matches what Storage.__init__ has been
    # hard-coding since the project began.
    cache_size: str = "1G"
    # WT's session_max — sized at 1000 to give generous headroom for
    # concurrent client connections + change-stream tailers. mongod
    # itself runs at 33000; the WT hard cap is much higher.
    session_max: int = 1000
    # TTL background sweeper interval (matches mongod's default 60s).
    # ``0`` disables the thread entirely — tests that drive expiry
    # deterministically via ``prune_ttl(now=...)`` use that escape
    # hatch.
    ttl_sweep_seconds: float = 60.0
    # When True: WT writes log records through ``fsync`` on every
    # commit (``transaction_sync=enabled=true,method=fsync``). This is
    # the "production durability" knob — closes the gap that makes
    # ``writeConcern: {j: true}`` silently best-effort today. Costs
    # 10-100x on small-doc insert throughput depending on the
    # underlying disk, so leave False unless you actually need it.
    sync_on_commit: bool = False

    # ---- [tls] ------------------------------------------------------
    # Paths to a PEM-format certificate chain and private key. When
    # both are set, the daemon wraps every accepted socket with TLS
    # before passing it off to the connection thread. Clients then
    # speak the mongo wire protocol over the encrypted channel; their
    # URIs need ``?tls=true`` (and ``tlsCAFile=`` to verify the
    # server cert against a CA they trust). When unset, the daemon
    # stays plaintext.
    tls_cert_file: str | None = None
    tls_key_file: str | None = None
    # mTLS (mutual TLS): when ``ca_file`` is set, the daemon asks
    # connecting clients for an X.509 certificate during the TLS
    # handshake and verifies it against this CA bundle. Only
    # meaningful when ``cert_file`` / ``key_file`` are also set —
    # configuring mTLS without server-side TLS is a deployment
    # mistake and raises at startup.
    #
    # ``require_client_cert=True`` rejects clients that don't
    # present a cert; ``False`` (default) verifies a cert if one is
    # offered but accepts clients without one (useful for staged
    # rollouts). MONGODB-X509 auth (cert-subject-as-username) is a
    # separate follow-on; this slice is the transport-layer gate
    # only.
    tls_ca_file: str | None = None
    tls_require_client_cert: bool = False


class ConfigError(Exception):
    """Raised when a TOML config file is malformed or references
    unknown keys. We fail loudly rather than silently ignoring typos
    — a misspelled ``cache_seize`` would otherwise look like the
    file works while the engine quietly runs on the default."""


# Map of TOML section → which field names live there. Used to apply a
# table-by-table update and to flag unknown keys cleanly.
_TABLE_FIELDS: dict[str, frozenset[str]] = {
    "server": frozenset({"host", "port", "storage_path", "log_level", "auth", "standalone"}),
    "oplog": frozenset({"retention_seconds", "max_entries", "noop_heartbeat_seconds"}),
    "storage": frozenset({"cache_size", "session_max", "ttl_sweep_seconds", "sync_on_commit"}),
    "tls": frozenset({"cert_file", "key_file", "ca_file", "require_client_cert"}),
}

# Some TOML keys are intentionally shorter than the dataclass field
# (``[oplog] retention_seconds`` → ``oplog_retention_seconds``) so
# the file reads naturally. The mapping below is the table-local
# rename layer.
_RENAMES: dict[tuple[str, str], str] = {
    ("oplog", "retention_seconds"): "oplog_retention_seconds",
    ("oplog", "max_entries"): "oplog_max_entries",
    ("oplog", "archive_dir"): "oplog_archive_dir",
    ("tls", "cert_file"): "tls_cert_file",
    ("tls", "key_file"): "tls_key_file",
    ("tls", "ca_file"): "tls_ca_file",
    ("tls", "require_client_cert"): "tls_require_client_cert",
}


def _parse(path: Path) -> dict[str, Any]:
    """Load a TOML file and return a flat ``{field_name: value}`` dict
    that ``replace(SecantusConfig(), **flat)`` can apply directly."""
    with path.open("rb") as f:
        try:
            data = tomllib.load(f)
        except tomllib.TOMLDecodeError as exc:
            raise ConfigError(f"{path}: invalid TOML: {exc}") from exc

    flat: dict[str, Any] = {}
    for table_name, allowed in _TABLE_FIELDS.items():
        table = data.get(table_name)
        if table is None:
            continue
        if not isinstance(table, dict):
            raise ConfigError(f"{path}: [{table_name}] must be a table, not {type(table).__name__}")
        for key, value in table.items():
            if key not in allowed:
                raise ConfigError(
                    f"{path}: unknown key [{table_name}].{key!r} (valid keys: {sorted(allowed)})"
                )
            field_name = _RENAMES.get((table_name, key), key)
            flat[field_name] = value
    # Flag any top-level tables the user thought we'd recognise.
    unknown_tables = set(data) - set(_TABLE_FIELDS)
    if unknown_tables:
        raise ConfigError(
            f"{path}: unknown top-level table(s): {sorted(unknown_tables)} "
            f"(valid: {sorted(_TABLE_FIELDS)})"
        )
    return flat


def discover_config_path() -> Path | None:
    """Walk the auto-discovery list and return the first existing path,
    or None if no config file is present anywhere on the path."""
    for candidate in _AUTO_DISCOVERY_PATHS:
        if candidate.is_file():
            return candidate
    return None


def load_config(explicit_path: Path | None = None) -> tuple[SecantusConfig, Path | None]:
    """Resolve and parse the TOML configuration.

    Returns ``(config, source_path_or_None)``. The source path is
    surfaced so the launcher can log "loaded config from X" — useful
    when ops staff are debugging which file actually got picked up.

    Raises ``ConfigError`` when an explicit path was passed but the
    file doesn't exist, or when the file is malformed.
    """
    if explicit_path is not None:
        if not explicit_path.is_file():
            raise ConfigError(f"config file not found: {explicit_path}")
        return replace(SecantusConfig(), **_parse(explicit_path)), explicit_path
    discovered = discover_config_path()
    if discovered is None:
        return SecantusConfig(), None
    return replace(SecantusConfig(), **_parse(discovered)), discovered


def apply_overrides(base: SecantusConfig, overrides: dict[str, Any]) -> SecantusConfig:
    """Apply explicit CLI flag overrides on top of the base config.

    ``overrides`` is the dict of ``{field_name: value}`` pairs the CLI
    actually saw (i.e. the user typed them). The launcher constructs
    it by checking each argparse attribute against ``None`` (its
    sentinel for "user did not pass this flag").

    Unknown field names raise — guards against the launcher and the
    config dataclass drifting apart.
    """
    valid = {f.name for f in fields(base)}
    bad = set(overrides) - valid
    if bad:
        raise ConfigError(
            f"apply_overrides: unknown field(s) {sorted(bad)} (valid: {sorted(valid)})"
        )
    return replace(base, **overrides)
