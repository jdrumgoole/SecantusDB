### The SQL server gets its ORM gauge — and a primary-key fidelity fix to go with it

SQLAlchemy's own dialect-compliance suite now runs against SecantusDB's
PostgreSQL server as a first-class conformance gauge (`invoke
validate-sqlalchemy`), joining the psycopg and sqllogictest gauges in the
weekly validation run — the sqllogictest gauge itself also graduates to weekly
CI in the same stroke. Nothing is vendored: the suite ships inside the
sqlalchemy package, pointed at a daemon server through the stock
`postgresql+psycopg` dialect, with SecantusDB's capabilities declared in a
requirements class the suite is designed to read. The opening baseline is 572
of 738 executed tests passing (77.5%), published in the new
`docs/validation-report-sqlalchemy.md`.

Standing the gauge up flushed out a real correctness bug: a table-level
`CONSTRAINT <name> PRIMARY KEY (…)` was silently dropped — the column was
never mapped to the document `_id`, so primary-key uniqueness was not
enforced and duplicate keys were accepted. Declared PK constraint names are
now honored end-to-end: enforcement, catalog reflection (in place of the
synthesized `<table>_pkey`), and duplicate-key error messages. The suite's
provisioning also forced two smaller statement gaps closed: `CREATE / DROP
EXTENSION` (citext, hstore, and plpgsql accepted — the extensions whose
functionality ships built in; anything else is honestly unavailable) and
`COMMENT ON CONSTRAINT` for check, unique, foreign-key, and primary-key
constraints.

#### Added

- `sqlalchemy_validation/`: the G6 ORM gauge of `tasks/sql-gauges-plan.md` —
  runner, capability declarations (`requirements.py`), report generator, and
  an `invoke validate-sqlalchemy` task; weekly in `validate.yml`.
- `.github/workflows/validate.yml`: the sqllogictest gauge (`validate-slt`)
  runs weekly too, with a pinned cached `sqllogictest-bin 0.29.1`.
- `sql/engine.py`: `CREATE EXTENSION [IF NOT EXISTS]` / `DROP EXTENSION
  [IF EXISTS]` for citext / hstore / plpgsql (no-op success); unknown
  extensions raise `0A000`, unknown drops `42704`.
- `sql/engine.py` + `sql/planner.py`: `COMMENT ON CONSTRAINT <c> ON <t>`
  (check / unique / FK / PK), stored in the catalog; `IS NULL` removes.

#### Fixed

- `sql/planner.py`: a table-level `CONSTRAINT <name> PRIMARY KEY (…)` was
  silently ignored — no `_id` mapping, no uniqueness enforcement. The PK now
  applies regardless of clause position, and the declared constraint name is
  recorded (`TableDef.pk_name`) and surfaced by `pg_constraint` /
  `pg_class` reflection and duplicate-key errors instead of the synthesized
  `<table>_pkey`.
