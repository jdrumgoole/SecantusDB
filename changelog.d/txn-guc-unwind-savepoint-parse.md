### Transaction-scoped GUC unwinding, and SAVEPOINT survives the parse guard

Three PostgreSQL-semantics fixes surfaced by re-baselining the pgjdbc
gauge after 0.6.0b11. A plain `SET` inside a transaction block now
unwinds on `ROLLBACK` (and on `COMMIT` of a failed block) while
surviving a successful `COMMIT`, exactly as PostgreSQL scopes it — and
whenever a `SET LOCAL` or rolled-back `SET` unwinds a GUC_REPORT
parameter, the server re-reports it via ParameterStatus so the client's
cached view reverts too (pgjdbc reads `getParameterStatus` straight
from that cache; all three of its transactionalParameters tests now
pass, and the extended-protocol Sync response delivers the revert for
autocommit implicit transactions).

The Parse-time bare-expression guard introduced with the pgx
parse-error work no longer rejects `SAVEPOINT name` / `RELEASE
SAVEPOINT name` — sqlglot parses both as a bare alias expression, which
the guard mistook for garbage, breaking JDBC's `Connection.setSavepoint`
outright. Bare `START TRANSACTION` (no characteristics tail) now opens
a block like `BEGIN`; only the `READ ONLY`-style suffixed forms parsed
before.

#### Fixed
- Plain `SET` in a transaction block: kept on COMMIT, unwound on
  ROLLBACK / failed-block COMMIT, with ParameterStatus re-reports.
- `SET LOCAL` unwind re-reports GUC_REPORT parameters (Sync-response
  delivery on the extended protocol).
- `SAVEPOINT` / `RELEASE SAVEPOINT` pass the extended protocol's
  garbage guard (pgjdbc setSavepoint regression, #876).
- Bare `START TRANSACTION` opens a transaction block.
