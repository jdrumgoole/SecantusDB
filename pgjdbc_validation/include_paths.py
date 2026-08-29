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
    # Excluded when cross-connection NOTIFY did not reach a blocked reader.
    # **That capability now works** (measured 2026-08-18): with one psycopg
    # connection blocked in `notifies()` — both with a timeout and with the
    # endless form, which is this exclusion's case — a `NOTIFY` issued on a
    # second connection is delivered. The read loop polls listeners on a 0.25s
    # `select` slice and flushes queued deliveries (`pgserver`
    # `_read_next_message`).
    #
    # Still excluded because that is not proof about *pgjdbc*: this test was
    # never re-run after the fix, and if it does still hang, a JUnit timeout
    # cannot interrupt the non-interruptible socket read — it would wedge the
    # whole weekly gauge for GRADLE_TIMEOUT_SECONDS rather than fail. To clear
    # it, delete this entry and run the gauge once: `uv run python -m
    # pgjdbc_validation.runner`.
    "NotifyTest": "cross-connection NOTIFY works (verified via psycopg); needs one pgjdbc run to confirm",
}
