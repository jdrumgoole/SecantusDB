### Admin UI: recent-connections order is deterministic under timestamp ties

`TargetStore.recent()` ordered by `last_used_at` alone; on Windows,
`time.time()`'s ~15.6ms resolution makes back-to-back records tie, so the
recent-targets list (and the trim that caps the table) could return them in
arbitrary order — caught as a Windows-only CI flake. Both queries now break
ties by `rowid DESC` (the later insert), pinned by a regression test that
freezes the clock.
