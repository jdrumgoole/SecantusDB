"""Which pgx packages the gauge runs, and test deselects.

Same model as the other gauges: the vendored submodule is never modified;
divergence lives here. ``PACKAGES`` starts with the two low-level packages
the gauges plan targets (pgconn + pgproto3 — the strict wire exercise); the
higher-level ``pgx`` package and stdlib adapter are later growth. ``SKIP_RUN``
is a Go ``-skip`` regexp for tests that hang or take the run down — ordinary
failures are the signal and stay in.
"""

PACKAGES = ["./pgconn/...", "./pgproto3/..."]

#: ``go test -skip`` regexp (empty = run everything).
SKIP_RUN = ""
