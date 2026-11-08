### fix_many starts a batch from main once the last batch has landed

#### Fixed

- `scripts/fix_many.sh -i <branch>` created the integration branch from
  `main` only when it did not exist. Once a batch had fully landed, the next
  batch's lanes started from the previous batch's tip, without anything
  `main` had gained since. Batch 13 (2026-11-08) started without the
  Backlog-ID reservation that had just merged. When the branch is an
  ancestor of `origin/main`, the script now fast-forwards it to `main`
  first. A branch holding work that hasn't landed yet is left alone.
