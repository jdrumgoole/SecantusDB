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
    # ----- Pure error-code unit tests (no driver client) -----
    # Mathematical assertions on the relationships between MongoDB
    # error-code categories (NotPrimary vs Shutdown vs Recovering
    # codes etc). Runs entirely client-side without contacting the
    # server, so passes regardless of what we implement.
    "test::error::custom_display",
    "test::error::not_writeable_primary_codes_disjoint_from_recovering_codes",
    "test::error::retryable_read_codes_differ_from_write_codes_by_exactly_134",
    "test::error::retryable_write_codes_subset_of_retryable_read_codes",
    "test::error::shutting_down_codes_subset_of_recovering_codes",
    # ----- Spec test runners that are client-internal -----
    # Handshake metadata append semantics, DNS SRV parsing — all run
    # in-driver without server roundtrips.
    "test::spec::handshake::append_metadata_",
    "test::spec::handshake::arbitrary_auth_mechanism",
    "test::spec::handshake::handshake_includes_backpressure_true",
    "test::spec::initial_dns_seedlist_discovery::load_balanced",
    # ----- CRUD spec helpers -----
    "test::spec::crud::generated_id_first_field",
    # The crud unified-spec runner drives ~80 subtests against the
    # full CRUD surface (find / insert / update / delete / aggregate
    # / countDocuments / distinct / findOne* / replaceOne /
    # bypassDocumentValidation / collation / hints / comments / let
    # bindings / readConcern levels / dots-and-dollars keys). Runs
    # end-to-end in ~75s.
    "test::spec::crud::run_unified",
    # ----- Change streams (single-node) -----
    # SecantusDB ships oplog-backed single-node change streams.
    # Driver tests that don't assume multi-node topology or real
    # transactions pass cleanly.
    "test::change_stream::tracks_resume_token",
    "test::change_stream::batch_end_resume_token",
    "test::change_stream::batch_mid_resume_token",
    "test::change_stream::errors_on_missing_token",
    "test::change_stream::empty_batch_not_closed",
    "test::change_stream::does_not_resume_aggregate",
    "test::change_stream::resume_uses_resume_after",
    "test::change_stream::resume_uses_start_after",
    "test::change_stream::resume_kill_cursor_error_suppressed",
    "test::change_stream::create_coll_pre_post",
    "test::change_stream::resumes_on_error",
    "test::change_stream::split_large_event",
    # ----- SCRAM authentication -----
    # SecantusDB ships SCRAM-SHA-256 end-to-end. The rust driver's
    # ``scram_sha256`` / ``scram_both`` tests exercise the full
    # round-trip against a daemon with auth on.
    "test::client::scram_sha256",
    "test::client::scram_both",
    # ----- Bulk write -----
    # Only ``unsupported_server_client_error`` actually exercises
    # wire behaviour against SecantusDB — it asserts that a server
    # reporting maxWireVersion below the bulkWrite threshold (which
    # we do) causes the driver to return its client-side
    # ``UnsupportedServer`` error. The other bulk_write tests gate
    # on ``server_version_lt(8, 0)`` and self-skip against
    # SecantusDB's ``buildInfo.version: "7.0.0"`` — they "pass"
    # without exercising anything, and the skip message line that
    # libtest emits separates the outcome onto its own line in a
    # way our pretty-format parser doesn't currently capture.
    # Bumping our reported version to 8.0 wouldn't help: the
    # ``bulkWrite`` command itself is a MongoDB 8.0 addition we
    # don't implement, so those tests would fail anyway.
    "test::bulk_write::unsupported_server_client_error",
    # ----- Spec runners: DNS SRV all topologies -----
    "test::spec::initial_dns_seedlist_discovery::replica_set",
    "test::spec::initial_dns_seedlist_discovery::sharded",
    # DNS SRV short-domain validation — all pure URI parsing,
    # client-side only.
    "test::spec::initial_dns_seedlist_discovery::short_srv_domains_invalid_end",
    "test::spec::initial_dns_seedlist_discovery::short_srv_domains_invalid_identical",
    "test::spec::initial_dns_seedlist_discovery::short_srv_domains_invalid_no_dot",
    "test::spec::initial_dns_seedlist_discovery::short_srv_domains_valid",
    # ----- Sessions spec -----
    # SecantusDB tracks logical sessions end-to-end (see
    # ``SessionRegistry``); the session-spec tests that don't gate
    # on multi-node topology / transactions pass against our
    # single-node deployment.
    "test::spec::sessions::implicit_session_after_connection",
    "test::spec::sessions::no_cluster_time_in_sdam",
    "test::spec::sessions::cannot_call_snapshot_time_on_non_snapshot_session",
    "test::spec::sessions::snapshot_and_causal_consistency_are_mutually_exclusive",
    "test::spec::sessions::snapshot_time_and_snapshot_false_disallowed",
    # ----- Handshake unified runner -----
    "test::spec::handshake::run_unified",
    # ----- Auth legacy runner -----
    # Exercises the legacy SCRAM-SHA-256 authentication path
    # against the daemon. SecantusDB ships SCRAM-SHA-256 end-to-end.
    "test::spec::auth::run_legacy",
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
# * test::spec::faas::*                    — the FaaS env-detection
#   tests share a process-wide `OnceLock` (TEST_METADATA) that the
#   first handshake sets; with libtest substring matching every
#   faas test runs in the same cargo invocation and asserts
#   against its own expected metadata, which fails because the
#   OnceLock is already pinned. Each test would need its own
#   cargo invocation in a fresh process — a future widening could
#   add the 8 filters individually, but they don't actually test
#   wire-protocol behaviour (driver-side env-var inspection), so
#   the conformance value is low.
# * test::spec::collection_management::run_unified — the second
#   subtest (`timeseries-collection.json::insertMany with duplicate
#   ids`) requires real time-series collections (bucket-based
#   storage that doesn't enforce ``_id`` uniqueness). SecantusDB
#   doesn't ship time-series; bringing it in is a multi-slice
#   feature, deferred.
# * test::spec::sessions::run_unified — the ``implicit-sessions-
#   default-causal-consistency`` test expects ``readConcern.level:
#   "snapshot"`` to succeed on a "replica set" topology. SecantusDB
#   advertises as a single-node replica set primary (so change
#   streams light up) but rejects snapshot RC with ``246
#   SnapshotUnavailable`` because we don't implement majority-
#   committed snapshots. Either supporting snapshot RC (large
#   feature) or stopping the replica-set advert (breaks change
#   streams) would unblock; deferred.
# * test::spec::command_monitoring::command_monitoring_unified
#   — the full unified-test spec runner for command monitoring;
#   the panic at ``operation.rs:202`` is the unified runner's
#   "operation type unsupported" path. Many subtests need
#   driver-specific operation handlers (``aggregate``,
#   ``listIndexes`` shapes, etc.) we'd want to verify case-by-case;
#   deferred.
# * test::change_stream::aggregate_batch    — start_after token
#   handling; the test asserts the stream's resume_token equals
#   the provided start_after value before any event has been
#   delivered. SecantusDB doesn't pre-populate the resume_token
#   from start_after; deferrable.
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
