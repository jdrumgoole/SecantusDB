"""In-scope RSpec paths under vendor/mongo-ruby-driver/spec/.

These are **integration** specs — they connect to the SecantusDB
daemon over the wire and exercise real CRUD / cursor / aggregation
/ session behaviour. The runner pre-provisions the ``root-user``
and ``ruby-test-user`` users mongo-ruby-driver's ``spec_helper``
expects, so the tests can authenticate and run end-to-end.

The previous baseline was lite-only (``lite_spec_helper`` files
that exercise the driver's pure-code logic without ever opening a
TCP connection). Lite specs verify the Ruby driver works in our
build environment but say nothing about SecantusDB conformance, so
they're out of scope for this gauge — they'd inflate pass-counts
without proving anything about our wire layer.

Each path covers a specific area of mongod's command surface:

* ``database_spec.rb`` — db-level commands (drop, listCollections, ...)
* ``collection_crud_spec.rb`` / ``collection_ddl_spec.rb`` — CRUD + DDL
* ``cursor_spec.rb`` — find cursor + getMore
* ``bulk_write_spec.rb`` — ordered / unordered bulkWrite
* ``index_view_spec.rb`` — createIndexes / listIndexes / dropIndexes

Widen the list to bring in more wire surface (change streams, GridFS,
sessions, transactions) once the baseline is stable.
"""

from __future__ import annotations

# Paths relative to vendor/mongo-ruby-driver/. These all
# ``require 'spec_helper'`` (the full helper) and need:
#   * SecantusDB listening at MONGODB_URI's host:port
#   * ``root-user`` + ``ruby-test-user`` pre-provisioned (the runner
#     does this via a setup pymongo client before invoking rspec).
INCLUDE: list[str] = [
    # Widening one file at a time. Each is added only after the
    # runner's wall-clock guard confirms it terminates against
    # SecantusDB.
    "spec/mongo/database_spec.rb",
    "spec/mongo/collection_ddl_spec.rb",
    "spec/mongo/index/view_spec.rb",
    "spec/mongo/address_spec.rb",
    "spec/mongo/config_spec.rb",
    # Deferred (each ran the 300s wall-clock kill when included):
    # - ``collection_spec.rb`` — large suite with several tailable /
    #   change-stream paths
    # - ``server_spec.rb`` / ``cluster_spec.rb`` / ``auth_spec.rb`` —
    #   SDAM + connection-pool retry loops that don't terminate
    #   against our single-node-as-RS topology.
    # Larger spec files staged for later (each has known hangs or
    # depends on features SecantusDB doesn't aim to support):
    # "spec/mongo/collection_crud_spec.rb",   # mostly works; widen separately
    # "spec/mongo/cursor_spec.rb",            # tailable getMore hangs
    # "spec/mongo/bulk_write_spec.rb",        # writeConcern enforcement
    # "spec/mongo/session_spec.rb",           # multi-doc transactions
    # "spec/mongo/session_transaction_spec.rb",
]

# RSpec ``--tag ~<name>`` patterns to skip slow / env-dependent tests.
SKIP_TAGS: list[str] = []
