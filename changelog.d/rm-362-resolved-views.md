### A read of a view names the views it resolved

The Rust MongoDB server now prints `resolvedViews` on the `Slow query`
line and in the profile entry of a read that goes through a view, as mongod
8.2 does. Each entry gives the view's namespace, its chain of names down to
the collection beneath it, and the pipeline it resolved to. A view defined
on another view lists the whole chain and the combined pipeline, innermost
view's stages first. A time-series collection counts as a view over its
buckets: its pipeline is mongod's `$_internalUnpackBucket` stage.

The list covers every view the command reads, not just the one it names:
the sources of `$lookup`, `$graphLookup` and `$unionWith`, the stages
inside `$facet` and nested pipelines, a `$out` or `$merge` target, and any
view named inside another view's own pipeline. Entries are ordered by name.
The server collects them at the same point mongod does, after it has
parsed the stages and before it parses their expressions. A read that
fails after that point still lists its views, and a `getMore` lists none.
Each of these cases was checked against mongod 8.2.1.

#### Fixed

- `secantus-commands`: `find`, `count`, `distinct`, `aggregate` and
  `explain` note the views their namespaces resolve to
  (`opdebug::note_resolved_views`), following mongod's
  `resolveInvolvedNamespaces`. `dispatch` hands them to the line and the
  profile entry. A `$out` or `$merge` target given as `{db, coll}` is
  looked up by its `coll` in the command's own database, as mongod does.
- `ts_buckets::unpack_stage` is now the one place that builds a
  time-series collection's `$_internalUnpackBucket` stage. `system.views`
  and `resolvedViews` both use it.
- `tests/mongod_live/test_slow_query_lines.py` no longer counts
  `resolvedViews` as unmodelled.
- Fixes RM-362
  (tests/backlog/test_rm_355_364.py::test_a_read_of_a_view_names_the_view_it_resolved,
  tests/backlog/test_rm_355_364.py::test_every_view_a_read_resolves_is_named).
- Filed RM-380: around a view, the line names the wrong namespace or kind
  (a `getMore`, a refused `findAndModify`, an `explain`, a read that
  fails once it runs). Filed RM-381: a `distinct` of a view has no
  `planningTimeMicros`.
