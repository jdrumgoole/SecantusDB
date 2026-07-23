"""Version drift: what's in the working tree vs what's been tagged.

Deliberately **local-only** (file reads + ``git tag``): no network, so this
panel always renders and never blocks the dashboard. The two servers version
independently — the Python server via ``src/secantus/__init__.py`` and ``vX.Y.Z``
tags, the Rust server via the crates' ``Cargo.toml`` and ``secantusdb-vX`` tags —
so drift is tracked per target.

Versions are read by regex, never by importing ``secantus`` (which would drag in
WiredTiger) or by parsing TOML (stdlib ``tomllib`` would do, but the version line
is trivially regexable and this keeps the reader uniform).
"""

from __future__ import annotations

import re
import subprocess
from collections.abc import Callable, Sequence
from dataclasses import dataclass
from pathlib import Path

Runner = Callable[[Sequence[str], str], tuple[int, str]]

_PY_VERSION_RE = re.compile(r'^__version__\s*=\s*["\']([^"\']+)["\']', re.M)
_CARGO_VERSION_RE = re.compile(r'^version\s*=\s*["\']([^"\']+)["\']', re.M)


def _git(argv: Sequence[str], cwd: str) -> tuple[int, str]:
    try:
        proc = subprocess.run(["git", *argv], cwd=cwd, capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.SubprocessError):
        return 1, ""
    return proc.returncode, proc.stdout


def _read(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8")
    except OSError:
        return ""


@dataclass(frozen=True)
class VersionInfo:
    target: str  # "python" | "rust"
    local: str  # version in the working tree ("" if unreadable)
    latest_tag: str  # most recent matching tag ("" if none)

    @property
    def drift(self) -> str:
        """'clean' when the tree matches the latest tag, else 'ahead'/'unknown'."""
        if not self.local or not self.latest_tag:
            return "unknown"
        return "clean" if self.latest_tag.endswith(self.local) else "ahead"


def python_version(repo_root: str | Path) -> str:
    text = _read(Path(repo_root) / "src" / "secantus" / "__init__.py")
    m = _PY_VERSION_RE.search(text)
    return m.group(1) if m else ""


def rust_version(repo_root: str | Path) -> str:
    # Any crate works — they're kept in lockstep — but prefer the server crate.
    for rel in ("crates/secantus-server/Cargo.toml", "crates/secantus-core/Cargo.toml"):
        m = _CARGO_VERSION_RE.search(_read(Path(repo_root) / rel))
        if m:
            return m.group(1)
    return ""


def latest_tag(repo_root: str | Path, pattern: str, *, runner: Runner | None = None) -> str:
    """Most recent tag matching ``pattern`` by version sort ('' if none)."""
    run = runner or _git
    code, out = run(["tag", "--list", pattern, "--sort=-v:refname"], str(repo_root))
    if code != 0:
        return ""
    for line in out.splitlines():
        if line.strip():
            return line.strip()
    return ""


def collect(repo_root: str | Path, *, runner: Runner | None = None) -> list[VersionInfo]:
    """Version + latest-tag for both independently-versioned servers."""
    return [
        VersionInfo(
            "python", python_version(repo_root), latest_tag(repo_root, "v*", runner=runner)
        ),
        VersionInfo(
            "rust",
            rust_version(repo_root),
            latest_tag(repo_root, "secantusdb-v*", runner=runner),
        ),
    ]


__all__ = ["VersionInfo", "collect", "python_version", "rust_version", "latest_tag"]
