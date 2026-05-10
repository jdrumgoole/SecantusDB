"""SQLite-backed store of recently-used target URIs.

Sister table to the console-history store (``history.py``); both live
in the same ``~/.secantus/admin.db`` file but in their own tables so
they can evolve independently.

Used by the ``/connection`` page to render a list of "switch to one of
these" buttons. Insert-or-update on every successful target swap;
``last_used_at`` becomes the ordering key.
"""

from __future__ import annotations

import sqlite3
import time
from contextlib import closing
from dataclasses import dataclass
from pathlib import Path
from typing import Any

MAX_TARGETS = 20


@dataclass(frozen=True)
class TargetEntry:
    uri: str
    last_used_at: float
    created_at: float


_SCHEMA = """
CREATE TABLE IF NOT EXISTS connection_targets (
    uri TEXT PRIMARY KEY,
    last_used_at REAL NOT NULL,
    created_at REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_targets_last_used
    ON connection_targets(last_used_at DESC);
"""


class TargetStore:
    def __init__(self, path: Path | str, *, time_func: Any = time.time) -> None:
        self.path = Path(path)
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self._time = time_func
        with closing(self._connect()) as conn:
            conn.executescript(_SCHEMA)

    def _connect(self) -> sqlite3.Connection:
        conn = sqlite3.connect(self.path)
        conn.row_factory = sqlite3.Row
        return conn

    def record(self, uri: str) -> None:
        if not uri or not uri.strip():
            return
        now = self._time()
        with closing(self._connect()) as conn:
            existing = conn.execute(
                "SELECT created_at FROM connection_targets WHERE uri = ?", (uri,)
            ).fetchone()
            if existing is None:
                conn.execute(
                    "INSERT INTO connection_targets (uri, last_used_at, created_at) "
                    "VALUES (?, ?, ?)",
                    (uri, now, now),
                )
            else:
                conn.execute(
                    "UPDATE connection_targets SET last_used_at = ? WHERE uri = ?",
                    (now, uri),
                )
            # Trim to the most-recent MAX_TARGETS so the table doesn't
            # grow without bound across years of dev work.
            conn.execute(
                """
                DELETE FROM connection_targets
                WHERE uri NOT IN (
                    SELECT uri FROM connection_targets
                    ORDER BY last_used_at DESC
                    LIMIT ?
                )
                """,
                (MAX_TARGETS,),
            )
            conn.commit()

    def forget(self, uri: str) -> None:
        with closing(self._connect()) as conn:
            conn.execute("DELETE FROM connection_targets WHERE uri = ?", (uri,))
            conn.commit()

    def recent(self, *, limit: int = MAX_TARGETS) -> list[TargetEntry]:
        with closing(self._connect()) as conn:
            rows = conn.execute(
                """
                SELECT uri, last_used_at, created_at
                FROM connection_targets
                ORDER BY last_used_at DESC
                LIMIT ?
                """,
                (max(1, int(limit)),),
            ).fetchall()
        return [
            TargetEntry(
                uri=str(r["uri"]),
                last_used_at=float(r["last_used_at"]),
                created_at=float(r["created_at"]),
            )
            for r in rows
        ]


__all__ = ["TargetStore", "TargetEntry", "MAX_TARGETS"]
