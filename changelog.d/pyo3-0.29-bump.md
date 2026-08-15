### PyO3 bindings on 0.29, clearing two RUSTSEC advisories

The three PyO3 binding crates (`secantus-core-py`, `secantus-server-py`,
`secantus-storage-py`) move from PyO3 0.22 to 0.29, clearing
RUSTSEC-2025-0020 (buffer overflow in `PyString::from_object`, fixed
≥0.24.1) and RUSTSEC-2026-0177 (missing `Sync` bound on
`PyCFunction::new_closure`, fixed ≥0.29.0). Neither vulnerable API was
called anywhere in the tree, so this was dependency-currency debt rather
than a reachable vector — but it retires the two advisories the
`cargo audit` CI gate had been baselining, so the gate now runs with an
empty ignore list and fails on any newly disclosed advisory.

Because the bindings were already written against PyO3's `Bound<'py, T>`
smart-pointer API, the migration was mechanical: the deprecated
`PyBytes::new_bound` / `get_type_bound` methods drop their `_bound` suffix
and `Python::allow_threads` becomes `Python::detach` (0.29's attach/detach
terminology). The `_secantus_core` engine bindings stay byte-for-byte
identical to pure Python — the full parity corpus (1705 cases) passes
unchanged.

#### Changed

- PyO3 bumped 0.22 → 0.29 across the three binding crates; `cargo audit`'s
  `--ignore RUSTSEC-2025-0020 --ignore RUSTSEC-2026-0177` entries removed
  (#584).

#### Security

- Cleared RUSTSEC-2025-0020 and RUSTSEC-2026-0177 (both in PyO3 <0.29).
