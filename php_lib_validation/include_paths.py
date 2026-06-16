"""In-scope PHPUnit test paths under vendor/mongo-php-library/tests/.

mongo-php-library defines a single ``Default Test Suite`` that points at the
whole ``tests/`` tree. Running it wholesale drags in suites that only make
sense against a real multi-node deployment — ``SpecTests`` and
``UnifiedSpecTests`` replay the cross-driver spec corpus (transactions,
retryable writes against a real RS, CSFLE, load-balancer fixtures), and
``GridFS`` / ``DocumentationExamplesTest`` lean on behaviours SecantusDB
intentionally doesn't emulate. They'd either hang on orchestration the gauge
can't provide or inflate the denominator with environment-gated skips.

So the gauge passes an explicit list of test directories to phpunit (phpunit
accepts paths positionally, overriding the configured testsuite). Each entry is
a directory of functional tests that opens a real TCP connection to the
SecantusDB daemon and exercises the wire protocol end-to-end — the same measure
the Ruby / Node / Java gauges report.

Widen one directory at a time, confirming it terminates within the runner's
wall-clock guard before committing it here.
"""

from __future__ import annotations

# Paths relative to vendor/mongo-php-library/. Passed positionally to
# ``vendor/bin/phpunit`` after the config is loaded, so only these run.
INCLUDE: list[str] = [
    # Core CRUD + DDL surface — the library's Operation classes wrap the
    # insert / update / delete / find / aggregate / createIndexes commands
    # SecantusDB implements. This is the heart of the conformance signal.
    "tests/Operation",
    "tests/Collection",
    "tests/Database",
    "tests/Command",
    # Pure-code model + query-builder + helper-function units. These don't
    # need a server but prove the library loads and runs against the
    # installed extension in our environment.
    "tests/Model",
    "tests/Builder",
    "tests/Functions",
    "tests/Comparator",
    # Deferred (need topology / orchestration SecantusDB doesn't provide,
    # or features out of scope — widen later as behaviour lands):
    # - tests/SpecTests, tests/UnifiedSpecTests (RS / txn / CSFLE corpus)
    # - tests/GridFS (chunked file storage)
    # - tests/DocumentationExamplesTest, tests/ExamplesTest
    # - tests/ClientFunctionalTest (SDAM / topology assertions)
]
