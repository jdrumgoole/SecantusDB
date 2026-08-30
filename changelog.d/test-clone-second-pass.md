### More of the test suite starts from a cloned database

An earlier change had tests copy a prebuilt WiredTiger database instead of
building a fresh one per test, but it only covered the files that named their
storage location in one particular way. The most common remaining form was the
same thing with a subdirectory, so this extends the same treatment to 54 more
files — cutting their combined runtime from 350 to 253 seconds.

Left alone deliberately: the backup, restore and point-in-time-recovery tests.
Those stand up several databases with distinct roles, and a restore target in
particular often needs to start empty, so handing them a pre-populated copy would
change what they actually prove.

#### Changed
- 54 further test files take the cloned-home fixture instead of creating a
  WiredTiger database per test.
