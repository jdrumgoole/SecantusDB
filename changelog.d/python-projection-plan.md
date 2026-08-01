### The Python server compiles a projection once per cursor, not once per document

Every projected document re-ran the whole projection front-end: meta
validation, spec partitioning, inclusion/exclusion mode detection, and —
worst — rebuilding the dotted-path trie from scratch, per row. The spec is
constant for a cursor's lifetime, so all of that now compiles once into a
projection plan and only the per-document work runs per document. Alongside
it, the expression engine stops shallow-copying the entire document on every
`$field` reference (the copy only existed to satisfy a type annotation — the
path walk is read-only), the matcher stops rebuilding a constant frozenset
per operator clause, and the pure-Python FNV shard-name hash is memoised.
Measured on the Python server: projected find drain +46%, exclusion
projection +19%, a `$group` pipeline +2.8%.

#### Changed
- `secantus.projection`: new `compile_projection` / `apply_projection_plan`
  split; `apply_projection` and the batch path are unchanged in behaviour
  (all seven Rust↔Python parity suites pass untouched — the Python engine
  stays the oracle).
- `secantus.expressions`: `$field` resolution no longer copies the document;
  `secantus.query`: `_SIBLING_MODIFIERS` hoisted to module scope;
  `secantus.storage`: shard-name lookup memoised, projected reads use the
  batch (compile-once) path.
