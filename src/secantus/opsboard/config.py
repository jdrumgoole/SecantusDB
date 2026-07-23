"""Ops Board configuration: layered resolution + a saveable config file.

Resolution order (highest wins):

    CLI flag  >  environment variable  >  saved config file  >  built-in default

Non-secret settings persist to a small JSON file (default
``~/.secantus/opsboard.json``) via ``--save``. The auth **token** is a secret
and is deliberately NOT stored here — it lives in its own ``opsboard-token``
file / ``SECANTUS_OPSBOARD_TOKEN`` env / ``--token`` flag (see ``cli``).

Every persistable setting also has an environment variable so the board can be
configured in a shell profile, a launchd/systemd unit, or CI without a config
file. The jobkit locations (``SECANTUS_OPSBOARD_DB`` / ``..._LOGS``) are the
same env vars jobkit itself reads, so a value set here reaches spawned
``./inv`` children too.
"""

from __future__ import annotations

import json
import os
from collections.abc import Mapping
from dataclasses import asdict, dataclass, fields
from pathlib import Path

# Environment variable names (single source of truth).
ENV_HOST = "SECANTUS_OPSBOARD_HOST"
ENV_PORT = "SECANTUS_OPSBOARD_PORT"
ENV_REPO_ROOT = "SECANTUS_OPSBOARD_REPO_ROOT"
ENV_NO_WINDOW = "SECANTUS_OPSBOARD_NO_WINDOW"
ENV_DB = "SECANTUS_OPSBOARD_DB"  # also read by jobkit
ENV_LOGS = "SECANTUS_OPSBOARD_LOGS"  # also read by jobkit
ENV_TOKEN = "SECANTUS_OPSBOARD_TOKEN"  # secret; handled in cli, not persisted
ENV_CONFIG = "SECANTUS_OPSBOARD_CONFIG"

DEFAULT_CONFIG_PATH = Path.home() / ".secantus" / "opsboard.json"


def _as_bool(value: object) -> bool:
    if isinstance(value, bool):
        return value
    return str(value).strip().lower() in ("1", "true", "yes", "on")


@dataclass
class OpsboardConfig:
    """The persistable (non-secret) Ops Board settings."""

    host: str = "127.0.0.1"
    port: int = 0  # 0 = pick a free port
    repo_root: str | None = None  # None → the checkout the board ships from
    no_window: bool = False  # True → headless (no pywebview window)
    db_path: str | None = None  # None → jobkit default (~/.secantus/opsboard.db)
    log_dir: str | None = None  # None → jobkit default

    # -- persistence ------------------------------------------------------

    def to_dict(self) -> dict[str, object]:
        return asdict(self)

    def save(self, path: str | Path) -> Path:
        path = Path(path)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(self.to_dict(), indent=2) + "\n", encoding="utf-8")
        return path

    @classmethod
    def load_file(cls, path: str | Path) -> dict[str, object]:
        path = Path(path)
        if not path.exists():
            return {}
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except (json.JSONDecodeError, OSError):
            return {}
        # Keep only known keys so an old/edited file can't inject junk.
        known = {f.name for f in fields(cls)}
        return {k: v for k, v in data.items() if k in known}

    # -- resolution -------------------------------------------------------

    @classmethod
    def resolve(
        cls,
        *,
        cli: Mapping[str, object] | None = None,
        env: Mapping[str, str] | None = None,
        config_path: str | Path | None = None,
    ) -> OpsboardConfig:
        """Merge defaults < saved file < env < CLI (non-None values only)."""
        env = os.environ if env is None else env
        path = config_path or env.get(ENV_CONFIG) or DEFAULT_CONFIG_PATH

        values: dict[str, object] = cls().to_dict()  # defaults
        values.update(cls.load_file(path))  # saved file

        # environment overlay
        if ENV_HOST in env:
            values["host"] = env[ENV_HOST]
        if ENV_PORT in env and env[ENV_PORT].strip():
            values["port"] = int(env[ENV_PORT])
        if ENV_REPO_ROOT in env:
            values["repo_root"] = env[ENV_REPO_ROOT]
        if ENV_NO_WINDOW in env:
            values["no_window"] = _as_bool(env[ENV_NO_WINDOW])
        if ENV_DB in env:
            values["db_path"] = env[ENV_DB]
        if ENV_LOGS in env:
            values["log_dir"] = env[ENV_LOGS]

        # CLI overlay — only keys the user actually passed (non-None).
        for key, val in (cli or {}).items():
            if val is not None and key in values:
                values[key] = val

        return cls(**values)  # type: ignore[arg-type]

    def export_env(self, env: dict[str, str] | None = None) -> dict[str, str]:
        """Write the jobkit-relevant locations into an env dict.

        Ensures the web app's Journal and any spawned ``./inv`` child agree on
        the journal/log locations. Returns the mutated dict (defaults to
        ``os.environ``).
        """
        env = os.environ if env is None else env
        if self.db_path:
            env[ENV_DB] = str(self.db_path)
        if self.log_dir:
            env[ENV_LOGS] = str(self.log_dir)
        return env


__all__ = [
    "OpsboardConfig",
    "DEFAULT_CONFIG_PATH",
    "ENV_HOST",
    "ENV_PORT",
    "ENV_REPO_ROOT",
    "ENV_NO_WINDOW",
    "ENV_DB",
    "ENV_LOGS",
    "ENV_TOKEN",
    "ENV_CONFIG",
]
