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
    # Verified terminates against SecantusDB in ~30 s with 90 passes / 7
    # failures / 15 pending out of 112 — full audit lives in
    # docs/validation-report-ruby.md.
    "spec/mongo/database_spec.rb",
    # Other ``spec/mongo/*_spec.rb`` integration files have at least one
    # test that hangs (tailable getMore, session-bound cursor wait,
    # change-stream resume, etc.). They're staged here as comments so
    # widening the gauge is just a matter of un-commenting one at a
    # time and confirming the runner's wall-clock guard doesn't trip.
    # "spec/mongo/collection_crud_spec.rb",
    # "spec/mongo/collection_ddl_spec.rb",
    # "spec/mongo/collection_spec.rb",
    # "spec/mongo/cursor_spec.rb",
    # "spec/mongo/bulk_write_spec.rb",
    # "spec/mongo/index/view_spec.rb",
]

# RSpec ``--tag ~<name>`` patterns to skip slow / env-dependent tests.
SKIP_TAGS: list[str] = []
