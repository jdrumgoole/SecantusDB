### Rust MongoDB server: short index reads cost what they did before operation counting, and less

Since RM-023 every read runs through the streaming executor so it can count
the keys and documents it examines. Profiling showed where that cost went:
the executor's index scan read 256 entries ahead of a ten-key range and
copied the database, collection and index names out of every entry, and
`find` planned its query a second time for a `planSummary` that only a
cursor left open needs. Against the build before RM-023 (release builds,
alternated round by round): an index-range `find` goes from +10% to -26%,
`count_documents` from +46% to +14%, `find_one` by `_id` from +17% to +12%.
Part of RM-365.

#### Changed

- An index scan's first read of the store takes 16 entries and doubles
  up to 256, instead of 256 every time; keys examined are unchanged (they
  count what the scan consumes).
- `Storage::index_entries_from` compares the key columns in place.
- `find` computes the cursor's `planSummary` only when the cursor outlives
  its first batch.
