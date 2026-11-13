### Rust MongoDB server: a `getMore` names the write concern its cursor was opened under

A `find` of a view or of a time-series collection runs as an `aggregate`,
and the `Slow query` line and profile entry of each of its `getMore`s named
the write concern an `aggregate`'s `getMore` runs under. mongod 8.2.11's name
none, as for a `find` of a collection: its `ClientCursor` keeps the write
concern of the command that opened it, and a `find` runs under none.

#### Fixed

- A cursor now keeps whether the command that opened it ran under a write
  concern, stamped at registration as its session and users are, and kept
  when a deferred cursor's pipeline reads on under the client's id. A
  `getMore` line or profile entry names a `writeConcern` only for such a
  cursor: an `aggregate`'s (of a collection or a view), never a `find`'s.
- Fixes RM-489 (tests/backlog/test_rm_view_namespaces.py::test_a_get_more_of_a_find_of_a_view_names_no_write_concern);
  also pinned by `test_a_get_more_names_the_write_concern_its_cursor_was_opened_under`
  beside it and the `cursors` unit test `a_cursor_keeps_its_openers_write_concern`.
