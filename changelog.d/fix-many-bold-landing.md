### fix_many accepts a landing reported in bold

#### Fixed

- `scripts/fix_many.sh` read `landed on <branch> at <sha>` only with the
  branch and sha bare or in backticks. A session that wrote the sha in bold
  (`at **2bc9d4dc**`) was counted as having landed nothing, and the batch
  stopped over a fix that was on the branch (2026-11-07, RP-449). Any
  markdown emphasis around either is accepted now; the sha must still be on
  the branch.
