### Views on the Rust MongoDB server are read-only, and read through

A probe of views against `mongod` 8.2.11 (`tools/probes/views.py`) found the
Rust MongoDB server different on 52 of 70 results. Several were wrong answers
with no error:

- an `insert`, `update` or `delete` aimed at a view was acknowledged, and
  stored or changed rows under the view's own name;
- `distinct` on a view returned nothing;
- `$lookup`, `$graphLookup` and `$unionWith` from a view matched nothing;
- `collMod` of a view's `pipeline` or `viewOn` answered `ok` and changed
  nothing.

It now differs on 2 of the 70, both the `system.views` collection, which the
server does not expose.

The same work found that `$count` over no documents returned `{n: 0}`.
`mongod` returns no document, and so does the Rust server now. The phantom
document also appeared in every `$lookup`, `$unionWith` and `$facet`
sub-pipeline that counted no match.

#### Fixed

- A view refuses writes (`insert`, `update`, `delete`, `findAndModify`),
  index commands, `collStats`, `validate`, `$collStats`, a change stream, a
  rename, and `$out` / `$merge` into it, with `mongod`'s errors.
- `distinct`, `$lookup`, `$graphLookup` and `$unionWith` read a view through
  its pipeline.
- `collMod` redefines a view: `pipeline` and `viewOn` each replace their own
  half. Collection options are refused on a view, and view options on a
  collection.
- `create` and `collMod` check a view's definition: a pipeline that is not an
  array, an unknown stage, `$out` / `$merge` / `$changeStream`, an empty
  `viewOn`, and a view defined on itself are refused. They were stored.
- `find` on a view accepts a `$natural` sort (it failed with a field-path
  error), refuses `tailable`, and refuses a collation other than the view's.
- Dropping a view answers `{ns, ok}` with no index count.
- `$count` over no documents emits no document.
