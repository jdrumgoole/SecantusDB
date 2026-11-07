### fix_many accepts a landing however it is worded

#### Fixed

- `scripts/fix_many.sh` looked for the words `landed on <branch> at <sha>`
  in a session's report. A session that wrote "the fix is on `pg-batch` at
  `2edf432c`" was counted as landing nothing, and stopped the batch over a
  fix that was on the branch (2026-11-07, RP-464). Any `on <branch> at
  <sha>` is accepted now; the sha must still be on the branch. The
  `fix-one` skill now says the line is read by a script and must be written
  exactly.
