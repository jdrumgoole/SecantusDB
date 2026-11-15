### Rust MongoDB server: a sort under `returnKey` is a `default` sort

A blocking SORT below a RETURN_KEY now explains as `type: "default"`, as
mongod 8.2.11's does: the sort keeps each member's index key, which
mongod's `simple` sort would discard.

#### Fixed

- `find(...).sort(...).returnKey()` explained its SORT as `simple`. The
  order of documents that tie under a limited sort is left as it was, per
  the scope decision that retired RM-049.

Fixes RM-566 (tests/backlog/test_rm_found_rm470.py::test_a_sort_under_return_key_is_a_default_sort)
