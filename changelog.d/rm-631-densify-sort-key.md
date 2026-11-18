### Rust MongoDB server: a `$densify` sets the sort key

A `$densify` now leaves each document it passes on the sort key of the
field it densified by, as mongod 8.2.11 does: `{$meta: "sortKey"}` reads
`[n]` and a rank behind it ranks by it. The documents it makes carry no key
at all, so `{$meta: "sortKey"}` is missing on them and a rank refuses them.

#### Fixed

- `{$meta: "sortKey"}` behind a `$densify` read `[]`, and a rank behind one
  tied every document.

Fixes RM-631 (tests/backlog/test_rm_found_2026_11_13_rm522.py::test_a_densify_sets_the_sort_key)
