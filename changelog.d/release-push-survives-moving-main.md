### A release no longer strands its tag when `main` moves during the test run

`release-prepare` pushed the release commit and the tag in one `git push`,
which is not atomic. When another PR merged during the 30-minute test run, the
tag was accepted and `main` was rejected, leaving a published tag on a commit
`main` did not have. It happened on `0.7.0b1` and again on `0.7.0b2`, and the
version bump had to be landed by hand each time.

#### Fixed

- `release-prepare` pushes `main` first and the tag only once `main` carries
  the release commit. If `main` moved, it merges `origin/main` in and retries;
  the tag stays on the commit the tests ran against.
- A merge conflict, or any other refused push, stops before the tag is pushed,
  so nothing is published.
