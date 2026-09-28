### `ORDER BY` over an expression, and a `WHERE` that cannot lower to a filter

Two more of the refusals the 2026-09-28 survey found. The predicate corpus
against PostgreSQL 14.13 goes 14/17 → **17/17**.

#### Added

- `ORDER BY <expression>` — `order by n * -1`, `order by upper(a)`, and several
  expression keys in one clause. The expression is materialised per row into a
  synthetic field just before sorting, so the comparison stays one routine.
- A `WHERE` that does not lower to an MQL filter (`where (case ... end)`) is
  evaluated per row as a residual instead of being refused. Only TRUE keeps a
  row, so SQL's three-valued logic is preserved.

#### Fixed

- **The build-provenance stamp went stale in a git worktree.** `build.rs`
  declared `rerun-if-changed=../../.git/HEAD`, but in a worktree `.git` is a
  FILE, so that path does not exist, cargo never re-ran the build script, and
  the stamp reported the tree of whichever checkout last built it — a staleness
  checker that is itself stale, which is the failure the block exists to
  prevent. All four stamping build scripts now resolve the path with
  `git rev-parse --git-path`.
