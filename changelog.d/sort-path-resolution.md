### A sort by `x.0` no longer reads an array as a number

When MongoDB sorts by a dotted path, it walks that path through arrays — and
what it does at the last step depends on how that step got there. An
array-valued key reached by a **field name** is descended one level; one reached
by an **index** is used exactly as it stands. Both servers descended in every
case, so `{x: [[5]]}` sorted by `x.0` ranked with the *numbers* (as `5`) instead
of with the *arrays* (as `[5]`). That is the wrong order — and, the moment a
`limit` is involved, the wrong documents.

Nothing had probed it. The engine-parity suites pin the Rust and Python engines
to each other, and here they were wrong together; the standing index probe
compares `_id` sets on purpose, so ordering stays out of its way. It took a
probe that compares ORDER against a real `mongod` — now
`tools/probes/sort_path_resolution.py` — to see it.

The same probe settled a second rule: mongod refuses, rather than guesses, a
sort path whose numeric component names both an array index and a key of that
array's elements.

#### Fixed

- **A sort key reached by an array INDEX is no longer descended.** `{x: [[5]]}`
  sorted by `x.0` sorts by `[5]`; `{x: [{y: [1, 2]}]}` sorted by `x.y` still
  sorts by `1`, because that step is a field name. Both servers, `find` and
  `$sort`, ascending and descending.
- **An ambiguous sort path is now the `16746` refusal mongod gives.** A
  component is ambiguous when it is a valid index of the array *and* some
  element document carries that exact key — so `{x: [{"0": 5}]}` sorted by `x.0`
  is refused, while `x.1` over `[{"1": 5}]` (index past the end) and `x.0` over
  `[{"00": 5}]` (`"00"` is not the key `"0"`) are answered normally. The
  element carrying the key need not be the one at that index. The same paths in
  a *filter* are still resolved both ways, as mongod resolves them.
- **The Rust aggregation `$sort` stage resolved its keys without walking
  arrays at all**, so it disagreed with the same server's `find` on 9 of 48
  measured shapes. Both Rust sort paths now share one resolver with the storage
  layer.
- **The Rust `find` handler reported this refusal under the update-side
  executor wrapper with an empty command name** (`Plan executor error during
  ::`). Read commands now get mongod's own `Executor error during find command:
  <db>.<coll> :: caused by ::`.

Measured against mongod 8.2.11 over 204 ambiguity shapes and 48 resolution
shapes: 0 divergent on both servers.
