"""Which pgjdbc test classes the gauge runs.

Same model as the other gauges: the vendored suite is never modified;
divergence lives here. ``INCLUDE_PACKAGES`` are java packages under
``org.postgresql.test`` whose ``*Test`` classes the runner enumerates from
the vendored tree (Gradle's ``--tests`` filter has no exclude form, so
per-class enumeration is what makes ``EXCLUDE_CLASSES`` effective). The
``jdbc2`` package is the core CRUD / statement / result-set suite the
gauges plan targets first; grow package by package (jdbc3, jdbc42, core)
as conformance grows.

``EXCLUDE_CLASSES`` is for hangs only — ordinary failures are the signal
and stay in.
"""

INCLUDE_PACKAGES = [
    "jdbc2",
]

#: Simple class names excluded from the run, with reasons.
EXCLUDE_CLASSES: dict[str, str] = {
    # Blocks forever in getNotifications(): a NOTIFY sent on connection B is
    # not delivered to connection A's endless-timeout poll — cross-connection
    # async notify delivery, tracked in tasks/backlog.md. A JUnit timeout
    # cannot interrupt the non-interruptible socket read.
    "NotifyTest": "cross-connection async NOTIFY delivery hangs the endless-timeout poll",
}
