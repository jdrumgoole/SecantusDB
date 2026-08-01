### The JDBC driver's own suite now measures the SQL server — and one fix moved it nine points

pgjdbc, the official PostgreSQL JDBC driver, joins the portfolio as the G5
gauge: `invoke validate-pgjdbc` runs the driver's own test suite —
unmodified, from a vendored submodule at REL42.7.13 — against a daemon
SecantusDB server. Targeting uses pgjdbc's stock `build.local.properties`
mechanism, which the project itself gitignores, so pointing the suite at us
leaves the vendored tree pristine. Scope opens at the `jdbc2` core package
(75 test classes, 5,500-odd tests) and grows package by package.

The opening baseline was 4,462 passed / 1,068 failed (80.7%) — and half of
those failures were a single protocol bug. Describe answered NoData for any
query with a CTE, then Execute sent DataRows anyway; pgjdbc refuses that
outright with "Received resultset tuples, but no field structure for them",
and a data-modifying CTE (`WITH x AS (INSERT … RETURNING …) SELECT * FROM x`)
tripped it every time. Describe now derives a CTE query's shape by planning
the outer SELECT against synthetic tables standing in for each CTE — the
data-modifying ones described from their RETURNING clause, nothing executed,
no side effects. That one fix took the gauge to **4,962 passed / 568 failed
(89.7%)**.

This is the third distinct form of the same protocol violation the SQL
gauges have surfaced this week (computed WHERE clauses, views, now CTEs),
each caught by a different client — which is exactly the argument for
running several strict drivers rather than one.

#### Added

- `pgjdbc_validation/` (runner with JDK-21 discovery, per-class enumeration
  so exclusions are effective, JUnit-XML aggregation, report generator),
  `vendor/pgjdbc` submodule at REL42.7.13, `invoke validate-pgjdbc`, and a
  weekly `validate.yml` row reusing the java/kotlin JDK + Gradle cache steps.

#### Fixed

- `sql/engine.py`: extended-protocol Describe reported NoData for every CTE
  query while Execute emitted rows — a protocol violation that made
  data-modifying CTEs unusable from strict clients.
