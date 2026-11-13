### A read of a view that does not parse is refused before the view is resolved

mongod parses a `find` -- its filter, its sort and its projection -- before
it resolves a view or a time-series collection, and parses an explained
`find` or `distinct` before it resolves one too. A read refused there is
logged and profiled under the view it names, `collectionType: "normal"`,
with no `resolvedViews`. This server parsed a `find`'s projection only after
it had resolved the view, and resolved the view before parsing any explained
read, so the refusal was filed under the collection beneath the view with
the views it had resolved, as a read that failed while running is.

#### Fixed

- `secantus-commands` (`find`): a `find`'s filter and projection are parsed
  before a view or a time-series collection is resolved, in mongod's order
  (filter, then projection), so a find of a view whose projection does not
  parse is refused naming the view.
- `secantus-commands` (`explain`): an explained `find` or `distinct`
  resolves its views only once it has parsed, as mongod's does; an explained
  `count` or `aggregate` still resolves them first, as mongod's does.

Fixes RM-483 (tests/backlog/test_rm_view_namespaces.py::test_a_read_refused_before_it_resolves_names_the_view); pinned further in tests/mongod_live/test_view_line_namespaces.py.
