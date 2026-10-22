### CI on a pull request: no Windows cell, no PGO or LTO in the test build

A pull request's CI took 40 to 51 minutes, set by one job: the Windows cell of
`storage-engine`, whose wheel build took 26 to 35 minutes on its own. Twelve of
those minutes were a single link of `_secantus_server`, the step that applies
thin LTO and the PGO profile. The tests that job runs check behaviour, not
speed, so a pull request now builds without either, and the Windows cell runs
on the push to `main` instead.

#### Changed

- `test.yml` `storage-engine`: the Windows cell runs on the push to `main` and
  on the weekly cron, not on a pull request. A Windows-only fault now shows up
  on `main`, after the merge.
- `storage-engine` on Linux, on a pull request and on the push to `main`,
  builds in a `fast` mode: `SECANTUS_PGO_DISABLE=1`, `lto = off`,
  `codegen-units = 16`. The full build users get (thin LTO, one codegen unit,
  the committed PGO profile) is still built and tested on every push to `main`
  by the Windows cell, and on all three OSes by the weekly cron.
- The cargo cache is keyed by build mode, restored on every run, and saved
  only from `main`, right after the build. A PR's cache was readable by that
  PR alone, and saving one cost up to 11 minutes on Windows. Before the save,
  this workspace's own crates are dropped from it: they recompile on every run
  anyway.
- The Windows cargo target root moved from `C:` to `D:`, the drive the test
  temp dir already uses, and the disk reports print `C:` as well.

#### Removed

- The `Reclaim disk (Windows)` step, which deleted the Android SDK on every
  Windows run (about 4 minutes). Moving the test temp dir to `D:` is what fixed
  the disk exhaustion it was added for.
