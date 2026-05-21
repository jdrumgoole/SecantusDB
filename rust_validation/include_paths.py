"""In-scope test targets under vendor/mongo-rust-driver/.

mongo-rust-driver organises its tests as ``#[cfg(test)]`` modules
inside ``driver/src/test/``. The cargo / libtest harness only
supports **one filter at a time**: passing multiple positional
filters either errors out (cargo's CLI) or ANDs the substrings
(libtest), neither of which gives a union. The runner therefore
invokes cargo once per entry in ``INCLUDE`` and aggregates the
results. The cargo target/ directory is reused across invocations
so the compile cost amortises to ~1-2 min once, then ~1s per
filter.

Each filter is treated as a libtest **substring** match, so
``test::client::list_databases`` runs both
``test::client::list_databases`` and
``test::client::list_database_names`` if both match the substring.
Use a more specific name (or a full ``test::client::list_databases$``
with ``--exact``) when that's a problem.

Cargo test name format: ``test::<module>::<test_name>`` for in-tree
tests. We currently only run in-tree (``--lib``) tests.

To list every available test:
    cd vendor/mongo-rust-driver && cargo test --lib -p mongodb -- --list
"""

from __future__ import annotations

# First-cut scope: a small curated set of handshake + single-collection
# CRUD tests that exercise the wire surface SecantusDB ships, without
# pulling in replica-set / transaction / encryption / load-balancer
# features. The list is intentionally small — the goal of this slice
# is the gauge plumbing, not a complete conformance run. Widen by
# adding specific test names as features expand or as failing tests
# get diagnosed and either fixed or annotated.
INCLUDE: list[str] = [
    # Client-level handshake + basic admin commands.
    "test::client::list_databases",
    "test::client::list_database_names",
    "test::client::metadata_sent_in_handshake",
    # Database-level commands.
    "test::db::list_collections",
    "test::db::list_collection_names",
    "test::db::collection_management",
    # Collection-level CRUD — using real test names from
    # driver/src/test/coll.rs.
    "test::coll::find",
    "test::coll::count",
    "test::coll::update",
    "test::coll::delete",
    "test::coll::empty_insert",
    "test::coll::ns_not_found_suppression",
]


# Cargo features to use. Default features are
# ``compat-3-0-0, rustls-tls, dns-resolver``. We don't need TLS or
# DNS for a localhost test, but the driver's default is what its own
# CI uses; sticking with it keeps test behaviour predictable.
CARGO_FEATURES: list[str] = []
