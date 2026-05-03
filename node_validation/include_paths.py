"""In-scope mocha test paths under vendor/node-mongodb-native/test/.

This baseline is **deliberately small** because of an unrelated tooling
quirk in mongo-node-driver v7.2.0: 68 of the unit test files contain
extensionless ESM imports like ``from '../../../mongodb'``. Node's ESM
resolver refuses to auto-pick `.ts`, and the driver's `.mocharc.js`
relies on `ts-node/register` which only handles CJS. mongo-node-driver
relies on a custom Node-version-conditional loader chain that doesn't
work cleanly with Node 22 + `npx mocha`. We could patch their
`.mocharc` or rewrite their imports, but both modifications defeat the
whole "unmodified upstream tests" point of the gauge.

So the include list is restricted to the test files that import only
from external packages and resolve cleanly under standard Node ESM.
That's still useful: bson serialization, connection-string parsing,
auth handshake, URI options, runtime adapters — the bulk of what the
node-driver gauge would catch in any case is BSON-shape regressions,
which `bson.test.ts` exercises thoroughly.

Widening this set requires solving the ESM/TS resolution problem
upstream — see node_validation/README.md for the loader options
attempted so far.
"""

from __future__ import annotations

# Paths relative to vendor/node-mongodb-native/.
INCLUDE: list[str] = [
    "test/unit/bson.test.ts",
    "test/unit/runtime_adapters.test.ts",
    "test/unit/connection_string.spec.test.ts",
    "test/unit/assorted/auth.spec.test.ts",
    "test/unit/assorted/imports.test.ts",
    "test/unit/assorted/uri_options.spec.test.ts",
]
