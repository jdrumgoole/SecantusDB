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

# Curated against the wire surface SecantusDB ships. Excludes tests
# that need replica-set retries, real transactions, sharding,
# encryption, Atlas search, x509 auth, or driver-side load-balancer
# behaviour — see EXPLICITLY_EXCLUDED at the bottom for the reasons.
#
# Substring matching by libtest means ``test::coll::find`` runs every
# test whose name contains it (``find_allow_disk_use``,
# ``find_do_not_allow_disk_use``, ``find_one_and_delete_*``, …). Most
# entries below intentionally rely on that fan-out — the explicit
# list captures the *category*, not every individual permutation.
INCLUDE: list[str] = [
    # ----- Client-level admin commands -----
    "test::client::list_databases",
    "test::client::list_database_names",
    "test::client::list_authorized_databases",
    "test::client::metadata_sent_in_handshake",
    "test::client::find_one_and_delete_serde_consistency",
    "test::client::saslprep",
    "test::client::server_address_from_socket_addr",
    # ----- Database-level commands -----
    "test::db::aggregate_with_generics",
    "test::db::collection_management",
    "test::db::create_collection_options_deserialize",
    "test::db::create_index_options_defaults",
    "test::db::db_aggregate",
    "test::db::deserialize_clustered_index_option_from_bool",
    "test::db::list_collection_names",
    "test::db::list_collections",
    "test::db::list_collections_filter",
    "test::db::test_run_command",
    # ----- Collection-level CRUD -----
    # ``find`` fans out across find / find_allow_disk_use /
    # find_do_not_allow_disk_use / find_allow_disk_use_not_specified /
    # find_one_and_delete_hint_*.
    "test::coll::find",
    "test::coll::count",
    "test::coll::count_documents_with_wc",
    "test::coll::cursor_batch_size",
    # ``delete`` fans out to delete + delete_hint_*.
    "test::coll::delete",
    "test::coll::drop_skip_serializing_none",
    "test::coll::empty_insert",
    "test::coll::aggregate_out",
    "test::coll::aggregate_with_generics",
    "test::coll::collection_generic_bounds",
    "test::coll::configure_human_readable_serialization",
    "test::coll::insert_err_details",
    "test::coll::insert_many_document_sequences",
    "test::coll::kill_cursors_on_drop",
    "test::coll::no_kill_cursors_on_exhausted",
    "test::coll::large_insert",
    "test::coll::ns_not_found_suppression",
    "test::coll::test_namespace_fromstr",
    "test::coll::typed_find_one_and_replace",
    "test::coll::typed_insert_many",
    "test::coll::typed_insert_one",
    "test::coll::typed_replace_one",
    "test::coll::typed_returns",
    "test::coll::update",
    # ----- Cursor lifecycle -----
    "test::cursor::batch_exhaustion",
    "test::cursor::borrowed_deserialization",
    "test::cursor::cursor_final_batch",
    "test::cursor::cursor_has_next",
    "test::cursor::session_cursor_next",
    "test::cursor::session_cursor_with_type",
    # ----- Index management -----
    # ``index_management_`` fans out across creates / drops / lists /
    # executes_commands / handles_duplicates / string_names.
    "test::index_management::index_management_",
    "test::index_management::commit_quorum_error",
]

# Tests excluded with reasons — kept here as documentation for the
# next gauge widening. Adding any of these requires either fixing a
# real SecantusDB-side gap or annotating it as an expected failure.
#
# * test::client::scram_*                 — SCRAM helper paths; we
#   ship SCRAM-SHA-256 but the rust driver tests probe specific
#   client-side helpers (sha1, missing_user_options) that fail in
#   driver-internal ways unrelated to the wire surface.
# * test::client::x509_auth_skip_ci       — X.509 auth + cert chain
#   handling (we ship MONGODB-X509 but the test fixture path needs
#   investigation).
# * test::client::connection_drop_during_read
#   test::client::operation_retry_uses_exponential_backoff
#   test::client::overload_errors_retried_*
#   test::client::server_selection_timeout_message
#   test::client::retry_commit_txn_check_out
#   test::client::backpressure_run_unified  — replica-set retries /
#   server-selection / backpressure heuristics.
# * test::client::end_sessions_on_*       — driver-side session
#   teardown ordering.
# * test::client::ipv6_connect            — needs IPv6 binding;
#   gauge picks ephemeral IPv4.
# * test::client::manual_shutdown_*       — driver runtime
#   shutdown semantics, not server-side.
# * test::client::warm_connection_pool    — pool warming heuristics.
# * test::coll::invalid_utf8_response     — depends on a controlled
#   binary response from a real mongod test fixture.
# * test::coll::collection_options_inherited — uses
#   ``ReadPreference::Secondary`` which only works on multi-node
#   replica sets; the test reads back the read-preference field
#   that gets stripped on standalone.
# * test::coll::large_insert_ordered_with_errors
#   test::coll::large_insert_unordered_with_errors
#   test::coll::no_read_preference_to_standalone
#                                         — replica-set / read-pref
#   behaviour the test asserts against.
# * test::cursor::tailable_cursor         — we ship change-streams
#   but the test's specific setup needs verification before
#   inclusion.
# * test::db::clustered_index_list_collections — we don't ship
#   clustered indexes (out of scope per CLAUDE.md).
# * test::db::db_aggregate_disk_use       — needs $listLocalSessions
#   reply shape on db.aggregate; deferred.
# * test::index_management::run_unified   — full unified-test spec
#   suite for index management; widens to 100+ subtests, each its
#   own surface to verify; deferred until a dedicated pass.
# * test::index_management::search_index_skip_ci::* — Atlas search;
#   out of scope.
# * test::change_stream::*                — single-node change
#   streams work, but the rust driver's tests assert oplog shapes
#   we'd want to verify case-by-case (transaction_fields needs txns
#   we don't ship; resume_* paths need careful checking).
# * test::bulk_write::*                   — bulk_write protocol;
#   needs its own slice — some tests assume max-batch behaviour
#   the gauge would have to set up explicitly.


# Cargo features to use. Default features are
# ``compat-3-0-0, rustls-tls, dns-resolver``. We don't need TLS or
# DNS for a localhost test, but the driver's default is what its own
# CI uses; sticking with it keeps test behaviour predictable.
CARGO_FEATURES: list[str] = []
