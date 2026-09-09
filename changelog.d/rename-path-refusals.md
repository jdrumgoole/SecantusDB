### `$rename` no longer rewrites paths MongoDB refuses

`$rename` will not move a field into or out of an array element, and it will not
accept a positional token in either path. Both servers accepted several of those
updates and applied them — and one of them, `{$rename: {"items.$[].a":
"items.$[].b"}}`, was renaming every element of the array, which MongoDB does
not do at all.

That last one had a test asserting it worked. The test drove our own server, the
only server it could reach, so it recorded what the code did rather than what
MongoDB does; it has been corrected along with the behaviour.

The two refusals differ in when MongoDB can decide them, and that changes how
they arrive: a **dynamic** component (`$`, `$[]`, `$[id]`) is a parse error,
raised without looking at the document and sent bare, while a path that
**indexes into an array** is discovered per document, arrives under `Plan
executor error during update :: caused by ::`, and is skipped altogether when
the source field is absent — because then the rename is simply a no-op.

#### Fixed

- **A dynamic component in a `$rename` path is refused** — `The source field for
  $rename may not be dynamic: <path>`, or its `destination` twin. Raised before
  the document is read, so an absent source still errors. Precedence, measured:
  source-dynamic before destination-dynamic, and both before an array-element
  path; the general `No array filter found for identifier` check comes first
  of all.
- **The array-element messages name the field holding the array and carry
  MongoDB's executor wrapper.** `deep.n.0.a` reports `'n'`, not the whole path,
  and the `_id` in the message uses MongoDB's value rendering — a string `_id`
  is quoted, an ObjectId is wrapped — where the Python server printed it raw.
- **A `$rename` whose source does not resolve is a no-op again**, so
  `{"v.9.a": "q"}` and `{"v.0.zz": "q"}` succeed rather than reporting a
  traverse failure.
- **`cannot use the part (…) to traverse the element` was sent bare by the
  Python server.** Every code-28 traverse failure carries the executor wrapper;
  the Rust server already did.
- **The Rust server failed the whole `update` command** when an `arrayFilters`
  identifier was missing, instead of recording one entry in `writeErrors`. A
  driver saw the wrong exception class, and an unordered batch lost the
  statements that had nothing wrong with them.

Measured against mongod 8.2.11 over 35 shapes: 0 divergent on both servers.
