"""``replSetStepDown`` — a single-node replica set's step-down window.

SecantusDB advertises itself as a single-node replica-set primary and already
answers ``replSetGetStatus`` with a full status, so it cannot honestly give a
standalone's ``76 NoReplicationEnabled`` here — that would contradict its own
handshake. What it can do is reproduce what a real single-node replica set
does, measured against mongod 8.2.11 on 2026-09-29:

=============================================  =========================================
request                                        answer
=============================================  =========================================
``{replSetStepDown: N, force: true}``          ``{ok: 1}``, then SECONDARY for N seconds
... during the window                          ``hello``: ``isWritablePrimary: false``,
                                               ``secondary: true``, no ``primary`` and
                                               no ``electionId``; WRITES fail
                                               ``10107 "not primary"``; READS work
``{replSetStepDown: N}`` with N <= catch-up    2 ``BadValue``
``{replSetStepDown: N}`` otherwise             262 "No electable secondaries caught up"
a negative period                              2 ``BadValue``
when already a secondary                       10107 "not primary so can't step down"
=============================================  =========================================

The 262 is the honest one: a single-node set genuinely has no electable
secondary to hand over to, so mongod's answer is already ours.

**Election TIMING is deliberately not reproduced.** mongod's return to primary
is driven by its election machinery, not the period alone — a period of 5 came
back after ~5s but a period of 0 took ~19s, because the node must still win an
election. Modelling that means modelling election timeouts, which is the
multi-node machinery this project puts explicitly out of scope. Here the window
is exactly the requested period.
"""

from __future__ import annotations

import threading
import time

#: mongod's default ``secondaryCatchUpPeriodSecs``.
DEFAULT_CATCH_UP_SECS = 10

#: mongod's code for "this node is not the primary".
NOT_PRIMARY = 10107


class StepDownError(Exception):
    """A ``replSetStepDown`` refusal carrying mongod's code and codeName."""

    def __init__(self, message: str, code: int, code_name: str) -> None:
        super().__init__(message)
        self.code = code
        self.code_name = code_name


class StepDownState:
    """Server-wide step-down window.

    ``topology_counter`` advances on every step-down. A driver IGNORES a "not
    primary" error whose ``topologyVersion`` is not newer than the one it
    holds, so a counter frozen at 0 would make the step-down look stale and
    never reach the driver.
    """

    def __init__(self, clock=time.monotonic) -> None:
        self._lock = threading.Lock()
        self._until: float | None = None
        self._counter = 0
        self._clock = clock

    def is_stepped_down(self) -> bool:
        with self._lock:
            return self._until is not None and self._clock() < self._until

    def topology_counter(self) -> int:
        with self._lock:
            return self._counter

    def step_down_for(self, seconds: float) -> None:
        with self._lock:
            self._until = self._clock() + seconds
            self._counter += 1


def not_primary_reply(state: StepDownState | None, process_id) -> dict:
    """The reply a write gets while this node is stepped down.

    Carries ``topologyVersion``, which mongod includes and drivers require:
    without it the driver marks the server UNKNOWN and the next READ fails
    server selection even though a secondary can serve it.
    """
    return {
        "ok": 0.0,
        "errmsg": "not primary",
        "code": NOT_PRIMARY,
        "codeName": "NotWritablePrimary",
        "topologyVersion": {
            "processId": process_id,
            "counter": state.topology_counter() if state is not None else 0,
        },
    }


def step_down(doc, *, replica_set_name: str | None, state: StepDownState | None) -> dict:
    """Apply ``replSetStepDown``, returning its reply or raising a refusal."""
    if replica_set_name is None:
        raise StepDownError("not running with --replSet", 76, "NoReplicationEnabled")
    if state is not None and state.is_stepped_down():
        raise StepDownError("not primary so can't step down", NOT_PRIMARY, "NotWritablePrimary")

    raw = doc.get("replSetStepDown")
    # mongod accepts a non-numeric or missing period under `force` (measured: a
    # string, a null and a double are all OK), so only a value that reads as a
    # NEGATIVE number is refused.
    period = raw if isinstance(raw, (int, float)) and not isinstance(raw, bool) else None
    if period is not None and period < 0:
        raise StepDownError("stepdown period must be a positive integer", 2, "BadValue")
    seconds = int(period) if period is not None else 0

    if not doc.get("force", False):
        catch_up = doc.get("secondaryCatchUpPeriodSecs", DEFAULT_CATCH_UP_SECS)
        if not isinstance(catch_up, (int, float)) or isinstance(catch_up, bool):
            catch_up = DEFAULT_CATCH_UP_SECS
        if seconds <= catch_up:
            raise StepDownError(
                "stepdown period must be longer than secondaryCatchUpPeriodSecs", 2, "BadValue"
            )
        # Past validation, a non-forced step-down needs a secondary to hand over
        # to. A single-node set has none -- and neither do we, so this is
        # mongod's own answer rather than a stand-in for one.
        now = time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime())
        raise StepDownError(f"No electable secondaries caught up as of {now}", 262, "Location262")

    if state is not None:
        state.step_down_for(seconds)
    return {"ok": 1.0}
