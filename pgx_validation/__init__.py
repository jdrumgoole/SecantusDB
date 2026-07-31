"""pgx (Go) conformance gauge for the SQL / PostgreSQL server.

The G4 gauge of ``tasks/sql-gauges-plan.md``: jackc/pgx's low-level packages —
``pgconn`` (hand-rolled connection / query / pipeline machinery) and
``pgproto3`` (wire-message codecs) — run **unmodified** from the vendored
submodule against a daemon ``SecantusPGServer`` via ``PGX_TEST_DATABASE``.
The strictest low-level Go client, the SQL analogue of the mongo-c-driver
gauge. Requires ``go`` on PATH.
"""
