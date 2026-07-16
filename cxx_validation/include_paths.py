"""Curated scope for the mongocxx (mongo-cxx-driver) conformance gauge.

The C++ driver's tests are Catch2 binaries. ``TEST_BINARIES`` is the set the
gauge builds and runs; ``test_driver`` is the broad CRUD / collection / cursor /
gridfs / command wire-protocol suite (the spec-runner binaries —
``test_transactions_specs`` / ``test_client_side_encryption_specs`` /
``test_retryable_reads_specs`` / ``test_unified_format_specs`` — are out of
scope: transactions, CSFLE, retryable writes and unified-format scenarios assume
a multi-node deployment / features SecantusDB doesn't emulate, so they're simply
not built or run).

``EXCLUDE_SPECS`` are Catch2 test-spec arguments passed to the binary to skip
out-of-scope test cases by tag / name (Catch2 excludes with a leading ``~``,
e.g. ``~[gridfs]``). Each entry carries a one-line rationale. Curate by building
``test_driver`` once and running it with ``--list-tests`` / ``--list-tags`` to
see the registered cases and tags.

Unlike libmongoc (which reads ``MONGOC_TEST_URI``), mongocxx's core tests
construct ``client{uri{}}`` — hard-wired to ``mongodb://localhost:27017`` with no
env override — so the gauge binds the SecantusDB daemon on port 27017 (see
``runner.py``) rather than an ephemeral port.
"""

from __future__ import annotations

# Catch2 test binaries to build and run, in order. Their JUnit output is merged
# into one raw artifact.
TEST_BINARIES: list[str] = [
    "test_driver",
]

# Catch2 test-spec arguments (exclusions). A leading ``~`` excludes the matching
# tag; passing only exclusions runs everything else. Each carries a rationale:
# the tag covers a feature SecantusDB doesn't emulate (multi-node topology,
# CSFLE, Atlas). Refine against ``test_driver --list-tags``.
EXCLUDE_SPECS: list[str] = [
    "~[client_side_encryption]",  # CSFLE / field-level encryption — out of scope
    "~[atlas]",  # Atlas-cloud-specific behaviour — out of scope
    "~[search_indexes]",  # Atlas Search indexes — not a mongod feature
    "~[transactions]",  # multi-document transactions assume a replica set
    "~[session]",  # causal-consistency / session tests assume multi-node
    "~[sdam_monitoring]",  # SDAM topology-monitoring assumes real topology
    "~[uri_options]",  # needs the URI_OPTIONS_TESTS_PATH spec-data dir; a URI
    # parsing fixture test, not a server-conformance test
]
