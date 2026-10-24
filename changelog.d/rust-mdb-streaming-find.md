### Rust MongoDB server: a `find` cursor reads on as `getMore` asks, and counts what each batch examined

A `find` read every matching document when it ran and kept the rest for
`getMore`, so a cursor was the result as it stood at that moment. mongod
reads on as the cursor is read: between batches it saves its position and
each `getMore` resumes on a fresh snapshot. A document inserted ahead of the
position appears, one deleted ahead of it does not, and one whose index key
moved ahead is returned again. The Rust server's `Slow query` lines and
profile entries also carried no `keysExamined` or `docsExamined`, since
nothing counted them.

#### Fixed

- `secantus-commands` (`explain/stream.rs`, `find.rs`, `cursors.rs`): a
  `find` streams over a forward collection scan, or a FETCH over an
  ascending, uncollated, non-multikey index other than `_id_`, with skip,
  limit and projection (measured against 8.2.11, 2026-10-24).
  - The cursor holds its plan and position, not documents: a RecordId for a
    collection scan, a key and RecordId for an index scan.
  - Each batch is read in the call that asks for it, inside a transaction
    on the transaction's snapshot.
  - A SKIP below the FETCH skips index entries without reading their
    documents, as mongod does.
  - A cursor another `getMore` is reading answers 292 `CursorInUse`.
  - The reads poll `maxTimeMS` and `killOp` where the whole-collection scan
    does, so a long streamed `find` still ends with 50 or 11601.
  - Every other plan keeps today's path; `tasks/streaming-cursors-plan.md`
    has the phases that follow.
- `secantus-storage`, `secantus-storage-adapter`: reads that resume a scan
  (`records_after`, `index_entries_from`) and fetch by RecordId
  (`records_by_id`). The PostgreSQL server links these crates and does not
  call the new reads.
- `secantus-core` (`sortkey.rs`): `value_len` / `split_compound` split a
  compound index key into its fields' keys by parsing the self-delimiting
  encodings. Splitting at the separator is not sound: a negative number's
  digit pair 99 encodes as `0x00`.
- `secantus-commands` (`slowop.rs`, `profiler/record.rs`): a streamed
  read's `Slow query` line and profile entry carry what its batch
  examined, with no lookahead. An index scan of 10 read 3 at a time
  examines 3, 3, 3, then 1. A `getMore` names the command that opened the
  cursor, repeats its `planSummary`, gives its `cursorid` on the last batch
  too, and says `cursorExhausted`. A profile entry for a `find` gives its
  `planSummary`.
- Fixes RM-080 for `find`
  (`tests/backlog/test_rm_057_084.py::test_get_more_sees_writes_made_since_the_find`,
  `::test_index_scan_get_more_sees_writes_made_since_the_find`) and the
  counts of RM-023
  (`tests/backlog/test_rm_001_028.py::test_slow_query_lines_count_each_batch`,
  `::test_profile_entries_count_each_batch`). Both items are trimmed:
  `aggregate` and the plans that do not stream yet remain, and so do
  RM-023's storage statistics.

#### Tests

- `tests/backlog/test_rm_185_locks.py::test_a_write_statement_logs_a_line_of_its_own`
  compares an index build's two `Slow query` lines without their order.
  mongod writes the build's line on its build thread, and under load it
  came after the command's.
