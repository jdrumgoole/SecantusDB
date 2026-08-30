### The mongod comparison suite runs on any 8.x server, not just one

The suite that compares SecantusDB against a real MongoDB server, operation by
operation, was pinned to the exact release its expectations were taken from. That
was deliberate caution when the expectations came from a server two majors behind,
but it meant a developer running any other build got the whole suite skipped and
no signal at all. MongoDB's error surface is stable within a major version, so the
suite now runs against any 8.x server and only skips across a major boundary.

The practical effect is that a difference on a nearby release now shows up as a
failure to investigate rather than a silent skip — which is the right default,
since a mismatch there is far more likely to be a real divergence than version
drift.

#### Changed
- The differential gate keys on the mongod major version rather than
  major-and-minor.
