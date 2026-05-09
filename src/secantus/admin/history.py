"""SQLite-backed query-history store for the ad-hoc console.

Each (uri, kind) combination keeps the last ``MAX_PER_URI`` entries
(default 50). Older entries are pruned on every record so the DB
stays bounded.

Designed to live at ``~/.secantus/admin.db`` alongside the persisted
admin token, but the path is a constructor argument so tests pass a
``tmp_path``.

Pure module — sqlite3 is stdlib, the connection is short-lived per
operation so concurrent admin processes don't fight a long-held lock.
"""

from __future__ import annotations

import sqlite3
import time
from contextlib import closing
from dataclasses import dataclass
from pathlib import Path
from typing import Any

MAX_PER_URI = 50

VALID_KINDS = ("find", "aggregate", "runCommand")


@dataclass(frozen=True)
class HistoryEntry:
    id: int
    kind: str
    payload: str
    created_at: float


_SCHEMA = """
CREATE TABLE IF NOT EXISTS recent_queries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    uri TEXT NOT NULL,
    kind TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_uri_created
    ON recent_queries(uri, created_at DESC);
"""


class HistoryStore:
    """Append-and-prune log of recent console submissions."""

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

    def record(self, uri: str, kind: str, payload: str) -> None:
        if kind not in VALID_KINDS:
            raise ValueError(f"unknown kind: {kind!r}")
        with closing(self._connect()) as conn:
            conn.execute(
                "INSERT INTO recent_queries (uri, kind, payload, created_at) VALUES (?, ?, ?, ?)",
                (uri, kind, payload, self._time()),
            )
            # Prune oldest beyond MAX_PER_URI.
            conn.execute(
                """
                DELETE FROM recent_queries
                WHERE uri = ?
                  AND id NOT IN (
                      SELECT id FROM recent_queries
                      WHERE uri = ?
                      ORDER BY created_at DESC, id DESC
                      LIMIT ?
                  )
                """,
                (uri, uri, MAX_PER_URI),
            )
            conn.commit()

    def recent(self, uri: str, *, limit: int = 20) -> list[HistoryEntry]:
        with closing(self._connect()) as conn:
            rows = conn.execute(
                """
                SELECT id, kind, payload, created_at
                FROM recent_queries
                WHERE uri = ?
                ORDER BY created_at DESC, id DESC
                LIMIT ?
                """,
                (uri, max(1, int(limit))),
            ).fetchall()
        return [
            HistoryEntry(
                id=int(r["id"]),
                kind=str(r["kind"]),
                payload=str(r["payload"]),
                created_at=float(r["created_at"]),
            )
            for r in rows
        ]


__all__ = ["HistoryStore", "HistoryEntry", "MAX_PER_URI", "VALID_KINDS"]
