### The statement after DDL no longer lists every relation to ask whether the session holds a temp one

Each statement asks whether the session holds a temporary relation, which
decides whether its temp namespace exists (PostgreSQL's `myTempNamespace`).
The answer scanned every relation key and then forced the deferred list of
relations for `regclass` -- every table, row type, index, view and sequence,
with their oids -- on every statement straight after DDL, even one that names
no relation. A temp sequence or a standalone temp composite, the only temp
relations outside the relation keys, is now flagged when that list is
deferred, so it is built only when a statement looks a relation up.

#### Fixed

- Whether the session holds a temp relation no longer lists every relation
  after DDL (RP-911 in part, `tests/backlog/test_rp_qualified_calls.py::test_temp_namespace_follows_transactions`).
