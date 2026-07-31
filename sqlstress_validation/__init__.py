"""pgbench + psql stress/smoke for the SQL / PostgreSQL server.

The G7 gauge of ``tasks/sql-gauges-plan.md``: unmodified ``pgbench`` drives
the full init cycle (DROP/CREATE TABLE, client-side COPY, ALTER TABLE ADD
PRIMARY KEY) and the TPC-B script in all three protocol modes (simple /
extended / prepared), plus a concurrent select-only lane; ``psql`` runs the
``\\d``-family catalog smoke. The invariant: any error or connection drop is
a real bug. Requires ``pgbench`` and ``psql`` on PATH (postgresql-contrib /
libpq tools).

Write lanes run single-client: under write contention WiredTiger's
optimistic concurrency surfaces PG-SERIALIZABLE-style 40001 serialization
failures, which pgbench < 15 treats as fatal (no ``--max-tries``); the
40001-retry model is documented in ``tasks/backlog.md``.
"""
