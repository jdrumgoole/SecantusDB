### A late cancel during the parse is filed as a watch item

RP-002's regression test checks that a cancel sent while a large statement is being parsed takes effect within 0.25 s. It failed once in CI on 2026-11-08: the cancel was honoured 0.65 s late. That matches RP-002's own pre-fix measurement on CI, about 0.6 s, so it looks like an intermittent return of the double parse that RP-002 removed. A rerun of the same commit passed, and so did the 30 runs before it. The case is filed as RP-571, with what to capture if it happens again.

#### Changed

- Filed RP-571, a watch item in the PostgreSQL backlog.
