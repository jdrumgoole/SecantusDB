### A dotted sort key skipped the documents it should have ranked

`sort({"x.y": 1})` over array-of-subdocument data came back in the wrong order.
mongod ranks `x: [{y: 1}]` among the documents that **have** an `x.y` — by 1,
its representative element — and both servers ranked it with the documents that
have none. Wrong order becomes wrong *results* as soon as a `limit` is
involved.

The sort was resolving the path with a resolver that deliberately does not walk
through an array; that behaviour is right for `$set` and projection and wrong
here. The array-descending resolver already existed for index-key generation
and already has mongod's semantics, including stopping at one level —
`x: [[{y: 5}]]` has no `x.y` on mongod either. Using it for the sort makes the
in-memory order agree with an index walk by construction, which is the property
that matters: an index must change speed, never results.

Nearly shipped with a regression, caught by re-measuring the *undotted* sort
against the pre-change binary rather than assuming only the dotted case had
moved: the representative-element rule was briefly applied twice, which put
`x: [[5]]` among the numbers instead of the arrays.

Still divergent and filed: a dotted **positional** component (`x.0`) is
ambiguous. mongod tries it as an array index *and* as a literal field name,
descends only for the second reading, and raises
`16746 Ambiguous field name found in array` when a sort hits both. That is a
separate piece of work in path resolution.

#### Fixed

- `secantus.ordering` / `secantus-storage`: a dotted sort key walks one array
  level, so `sort({"x.y": 1})` ranks array-of-subdocument values by their
  representative element — the minimum ascending, the maximum descending — and
  a two-level path still counts as absent. The undotted sort is unchanged, and
  a unit test pins that it descends exactly once.
