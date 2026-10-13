### Rust MongoDB server: documents come back in the order mongod returns them

A query with no sort has no promised order, and a client still sees one:
the order of the index the server read. That order also decides which
document a `limit: 1` delete removes and which one an update without
`multi` changes. The Rust MongoDB server read through its own index router
and returned its own order, so the same write could change a different
document from mongod's.

It now takes the order from the plan mongod runs. The store says which
documents match, and the planner behind `explain` says in what order mongod
reads them.

Found by sending 6,531 commands to the server and to mongod 8.2.11 and
comparing the `_id`s each read returned, in order, and what each write of
one document left behind. 1,189 differed; 97 do now (`tasks/backlog.md`
7.080).

#### Fixed

- `{a: {$gt: 1, $lt: 9}}` over an index on `a` did not return `{a: [1,
  9]}`. One element is over 1 and another under 9, and the scan read only
  the keys between the two.
- `{"p.q": null}` over an index on `p.q` did not return a document whose
  array `p` has an element without `q`.
- A sort by `p.q` ranked that document by its other elements. mongod ranks
  it as null.
- `find`, the front of an `aggregate` pipeline, `distinct` and
  `findAndModify` return documents in the order mongod's plan reads them:
  an index's order for `$ne`, `$nin`, `$exists`, `$type`, `$not`, `$mod`
  and a regex, `_id` order under a hinted `_id` index, and equal keys last
  first on a backward scan.
- `delete` with `limit: 1`, `update` without `multi` and `findAndModify`
  change the first document in that order. `findAndModify` and `update`
  now read their `hint`.

#### Changed

- A read whose order the plan has to work out costs more than it did.
  Measured against the build before, on 20,000 documents: a read through
  the index the router also picks, a collection scan and a read by `_id`
  cost the same; `{a: {$ne: 5}}` over an index on `a` costs about three
  times as much, since mongod reads it in the index's order and the router
  scans the collection for it. A sort is unchanged unless documents compare
  equal.
