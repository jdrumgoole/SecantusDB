### Unique-key claims no longer survive their table

The storage-backed unique-index enforcement introduced a week ago kept its
claims table alive across namespace teardown: dropping a table (or index, or
database) left the dropped namespace's unique-key claims behind, so
recreating the table and inserting a previously-used value was falsely
rejected as a duplicate. Caught by the weekly conformance sweep — the
sqllogictest corpus cycles drop/create with unique indexes constantly — and
reproduced in eight lines. Every teardown path now releases the namespace's
claims: drop table, drop index, drop all indexes, drop database, and rename.

#### Fixed

- `secantus.storage`: `table:secantus_unique_keys` rows are purged wherever
  their index or collection dies. Pinned by per-path regression tests
  (`TestClaimsDieWithTheirNamespace`) and the previously-failing
  `index/delete` sqllogictest file, which passes again in both protocols.
