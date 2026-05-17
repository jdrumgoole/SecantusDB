"""Server-wide ``configureFailPoint`` registry.

Real ``mongod`` exposes a debug command, ``configureFailPoint``, that
test harnesses use to inject errors at well-known sites. The MongoDB
driver test suites (mongo-go-driver, the spec tests, etc.) lean on it
heavily — the cursor / database test packages all open by saying "set
a ``failCommand`` failpoint that fails ``insert`` / ``getMore`` /
``killCursors`` with code 100, then prove the driver surfaces it as a
``mongo.CommandError``."

SecantusDB only needs the slice of this surface those tests exercise.
The registry below stores active ``failCommand`` entries server-wide,
applied in ``dispatch`` before the real command handler runs:

* ``mode: "alwaysOn"`` → triggers indefinitely until disabled
* ``mode: {times: N}`` → triggers the next ``N`` matching commands
* ``mode: {skip: N, times: M}`` → skip ``N`` matches, then trigger ``M``
* ``mode: "off"`` (or absent / N=0) → disabled

Two trigger shapes are supported:

* ``data.errorCode`` → the matched command short-circuits with
  ``{ok: 0, code: errorCode, errmsg, codeName}``.
* ``data.writeConcernError`` → the matched command runs normally and
  the server appends a ``writeConcernError: {code, errmsg, codeName}``
  block to the response (mongod's gauge for "the operation succeeded
  but its replication concern failed").

Both shapes can carry an optional ``failCommands: [...]`` filter; an
empty list means "any command".
"""

from __future__ import annotations

import threading
from dataclasses import dataclass
from typing import Any


@dataclass
class _FailCommand:
    """A single configured ``failCommand`` failpoint."""

    fail_commands: tuple[str, ...]
    """Names this failpoint fires on. Empty == any command."""

    error_code: int | None = None
    """If set, return ``{ok: 0, code: errorCode}``."""

    write_concern_error: dict[str, Any] | None = None
    """If set, run the command then attach this block to the reply."""

    error_labels: tuple[str, ...] = ()

    close_connection: bool = False
    """If True, drop the TCP connection abruptly when matched."""

    block_connection: bool = False
    """If True, sleep for ``block_time_ms`` before processing the command."""

    block_time_ms: int = 0
    """Duration of the block in milliseconds (only honoured when
    ``block_connection`` is True). mongo-node-driver's CSOT explain
    tests use ``blockTimeMS: 2000`` so the client-side ``timeoutMS:
    1000`` timer fires first, surfacing as ``MongoOperationTimeoutError``.
    """

    times_remaining: int | None = None
    """``None`` == ``alwaysOn``; an int counts down to zero."""

    skip_remaining: int = 0
    """Number of matching commands to skip before triggering."""


@dataclass
class FailPointMatch:
    """The decision the registry returns for a single command."""

    error_code: int | None = None
    error_labels: tuple[str, ...] = ()
    write_concern_error: dict[str, Any] | None = None
    close_connection: bool = False
    block_connection: bool = False
    block_time_ms: int = 0


class CloseConnectionRequested(Exception):
    """Failpoint asked us to drop the connection. Caught in server.py."""


class FailPointRegistry:
    """Thread-safe per-server registry of active failpoints.

    Only ``failCommand`` is implemented. Other failpoint names are
    silently accepted (so test setup doesn't break) but never fire —
    real mongod exposes dozens of failpoints, almost all of which only
    a developer with a debug build of the server cares about.
    """

    def __init__(self) -> None:
        self._lock = threading.RLock()
        self._fail_commands: list[_FailCommand] = []

    def configure(self, name: str, mode: Any, data: dict[str, Any]) -> None:
        """Install / replace / disable a named failpoint.

        ``mode`` is what mongod accepts — ``"alwaysOn"``, ``"off"``,
        ``{"times": N}``, or ``{"skip": N, "times": M}``.
        """
        if name != "failCommand":
            # Accept-but-ignore: lets test setup that configures
            # SecantusDB-irrelevant failpoints (failGetMoreAfterCursorCheckout
            # etc.) keep going without a CommandNotFound.
            return

        with self._lock:
            self._fail_commands.clear()
            if mode == "off":
                return

            times: int | None
            skip = 0
            if mode == "alwaysOn":
                times = None
            elif isinstance(mode, dict):
                if "times" in mode:
                    raw_times = mode.get("times", 0)
                    times = int(raw_times) if raw_times is not None else 0
                else:
                    times = None
                raw_skip = mode.get("skip", 0)
                skip = int(raw_skip) if raw_skip is not None else 0
                if times == 0 and skip == 0:
                    return
            else:
                # Unknown mode shape; bail without installing.
                return

            fc = _FailCommand(
                fail_commands=tuple(data.get("failCommands") or ()),
                error_code=int(data["errorCode"]) if "errorCode" in data else None,
                write_concern_error=(
                    dict(data["writeConcernError"])
                    if isinstance(data.get("writeConcernError"), dict)
                    else None
                ),
                error_labels=tuple(data.get("errorLabels") or ()),
                close_connection=bool(data.get("closeConnection", False)),
                block_connection=bool(data.get("blockConnection", False)),
                block_time_ms=int(data.get("blockTimeMS", 0) or 0),
                times_remaining=times,
                skip_remaining=skip,
            )
            self._fail_commands.append(fc)

    def match(self, command_name: str) -> FailPointMatch | None:
        """Return the failpoint match for this command, if any.

        Consumes one ``times`` slot when a match fires. Returns
        ``None`` if no failpoint applies.
        """
        with self._lock:
            for fc in self._fail_commands:
                if fc.fail_commands and command_name not in fc.fail_commands:
                    continue
                if fc.skip_remaining > 0:
                    fc.skip_remaining -= 1
                    continue
                if fc.times_remaining is not None:
                    if fc.times_remaining <= 0:
                        continue
                    fc.times_remaining -= 1
                return FailPointMatch(
                    error_code=fc.error_code,
                    error_labels=fc.error_labels,
                    write_concern_error=fc.write_concern_error,
                    close_connection=fc.close_connection,
                    block_connection=fc.block_connection,
                    block_time_ms=fc.block_time_ms,
                )
            return None
