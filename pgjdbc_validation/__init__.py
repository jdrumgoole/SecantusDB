"""pgjdbc (Java) conformance gauge for the SQL / PostgreSQL server.

The G5 gauge of ``tasks/sql-gauges-plan.md``: the official PostgreSQL JDBC
driver's own test suite, run **unmodified** from the vendored submodule
(``vendor/pgjdbc``) against a daemon ``SecantusPGServer``. Targeting is the
driver's stock mechanism — a ``build.local.properties`` file, which pgjdbc
itself gitignores (``*.local.properties``), so writing it at gauge time
leaves the submodule pristine.

Scope starts with the ``jdbc2`` core package (the CRUD / statement /
result-set heart of the suite) per ``include_paths.py`` and grows package by
package, the same way the pymongo gauge's include list grew. A JUnit 5
default timeout is injected via system property so a hanging test costs 60
seconds, not the run.

Requires a JDK 21 toolchain (Gradle toolchain requirement of pgjdbc's
build): honored from ``JAVA_HOME`` when it is a 21, else discovered via
``/usr/libexec/java_home -v 21`` (macOS) or the homebrew ``openjdk@21``
path.
"""
