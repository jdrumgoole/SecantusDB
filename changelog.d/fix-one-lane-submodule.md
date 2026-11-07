### fix-one's lane mode updates the WiredTiger submodule over a local path

#### Changed

- The `fix-one` skill's lane-mode setup runs `git -c
  protocol.file.allow=always submodule update --init vendor/wiredtiger`.
  This machine's checkout points the submodule at a local path, which git
  refuses by default, and a lane session (RP-303) had to work that out
  before `./inv sync` would run.
