### A fix-one lane no longer re-gates when the other lane's commit only shares backlog prose

In integration mode a session re-runs its gate after rebasing whenever the commits it brought in touch a file its own commit touches. Every item edits the backlog file, and most add a changelog fragment, so the slower of two lanes re-gated after every landing by the faster one. On 2026-11-08 RM-332 rebased and re-gated three times. The fix-one skill now treats `tasks/backlog-rust-*.md` and `changelog.d/` as prose. When they are the only files both sides touch, the session runs the backlog markers test, which catches two lanes filing the same ID, and its own item's verify, then pushes. An overlap or conflict in code or tests still re-gates.

#### Changed

- Fix-one skill: the overlap test after a rebase ignores backlog prose and changelog fragments.
