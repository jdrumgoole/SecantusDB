"""In-scope mocha test paths under vendor/node-mongodb-native/test/.

These are **integration** tests under ``test/integration/`` — every
test opens a real ``MongoClient`` against the SecantusDB daemon and
exchanges wire commands end-to-end. The runner pre-provisions the
``root-user`` user before invoking mocha and exports ``MONGODB_URI``
with credentials, so the driver authenticates and the tests run
against a fully-authenticated session.

The previous baseline was ``test/unit/*`` files only — those exercise
the driver's pure-code paths (BSON encoding, URI parsing, etc.)
without ever opening a TCP connection. They don't measure SecantusDB
conformance, so they're out of scope for this gauge.

Each path covers a specific area of mongod's command surface. New
files are added one at a time after the runner's wall-clock guard
confirms they terminate against SecantusDB.
"""

from __future__ import annotations

# Paths relative to vendor/node-mongodb-native/. These all live under
# ``test/integration/`` and run via ``mocha --config test/mocha_mongodb.js``,
# which is the seam through which the test framework expects an actual
# ``mongod`` (or in our case, SecantusDB) at ``MONGODB_URI``.
INCLUDE: list[str] = [
    # Widening one file at a time — each verified to terminate
    # within the runner's 600 s wall-clock guard before being
    # added. The full ``test/integration/crud/`` set has ~20 files;
    # blind-adding all of them surfaces tailable / change-stream
    # hangs that pin the gauge, so we stage them in batches.
    "test/integration/crud/crud_api.test.ts",
    "test/integration/crud/abstract_operation.test.ts",
    "test/integration/crud/aggregation.test.ts",
    "test/integration/crud/document_validation.test.ts",
    "test/integration/crud/explain.test.ts",
    "test/integration/crud/find.test.ts",
    "test/integration/crud/find_and_modify.test.ts",
    "test/integration/crud/insert.test.ts",
    "test/integration/crud/remove.test.ts",
    "test/integration/crud/stats.test.ts",
    "test/integration/crud/unicode.test.ts",
    "test/integration/crud/server_errors.test.ts",
]
