"""CockroachDB pgtest wire-protocol gauge for the SQL / PostgreSQL server.

The G3 gauge of ``tasks/sql-gauges-plan.md``: the closest existing thing to a
pgwire protocol conformance suite — ~54 datadriven files of raw
Parse/Bind/Describe/Execute/COPY/error exchanges from CockroachDB's
``pkg/sql/pgwire/testdata/pgtest``, driven by CockroachDB's own
``pkg/testutils/pgtest`` runner **verbatim** over jackc/pgproto3. The SQL
analogue of the mongo-c-driver / php-ext gauges: message-level framing
checks, with the corpus' built-in surrogate tolerances (``crdb_only`` /
``noncrdb_only`` gating — we present as non-crdb — and ErrorResponse details
blanked unless ``keepErrMessage``).

Neither the corpus nor the runner is vendored: ``runner.py`` fetches both
from a **pinned** cockroach commit at gauge time via a sparse, blob-filtered
clone (~tens of MB, cached under ``.validation/``) — the same
fetch-at-run-time pattern as the SLT gauge's ``cargo install``. This keeps
the multi-gigabyte monorepo out of the repo and the CockroachDB Software
License (source-available; fine to *use* for dev-only testing, flagged here
rather than shipped) out of the tree. The only committed Go code is ours: a
thin ``go test`` driver and a 10-line ``skip`` shim the upstream runner
imports (``pgtest_validation/go/``).

Requires ``go`` and network access (first run) on PATH.
"""

#: The pinned cockroach commit the corpus + runner are fetched at. Bump
#: deliberately; the gauge measures SecantusDB, not corpus drift.
CRDB_COMMIT = "e3bff5d92ac171e3c45a0eb6cda5356b4182e4ed"
