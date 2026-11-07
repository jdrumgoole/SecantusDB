### A fix-one batch no longer stops on a landing reported in backticks

`scripts/fix_many.sh` checks that each integration-mode session landed a commit, by reading `landed on <branch> at <sha>` from the session's report. The report is markdown, and the first real MongoDB session wrote ``landed on `rm-batch` at `b2ab0554` ``. The check did not recognise the backticks, so it stopped the batch over a commit that had landed. Backticks around the branch and the sha are now accepted. Each batch also writes a start line to `summary.txt`, so its lines can be told apart from the previous batch's.

#### Fixed

- `scripts/fix_many.sh`: the landing check accepts the branch and sha in backticks.

#### Changed

- `scripts/fix_many.sh`: each batch writes a `START` line to `.scratch/fix_many/summary.txt`.
