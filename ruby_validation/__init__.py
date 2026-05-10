"""mongo-ruby-driver validation against an embedded SecantusDB daemon.

Same daemon-subprocess shape as ``go_validation`` and
``node_validation``: spawn ``python -m secantus`` as a subprocess,
point the driver's RSpec suite at it via ``MONGODB_URI``, run a
curated subset of unit / lite specs, then turn the JSON output into
``docs/validation-report-ruby.md``.

Run via ``uv run python -m invoke validate-ruby``. Requires Ruby 2.7+
and ``bundler`` on PATH.
"""
