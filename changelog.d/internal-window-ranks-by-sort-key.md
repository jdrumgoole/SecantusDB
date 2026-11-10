### A pipeline's own `$_internalSetWindowFields` ranks by the documents' sort key

On the Rust MongoDB server, `$rank` and `$denseRank` in a
`$_internalSetWindowFields` that a pipeline names itself now compare the sort
key a `$sort` in front attached to each document, as mongod 8.2 does, rather
than the stage's own `sortBy` values. With no `$sort` in reach every document
ties at rank 1. Behind a `$sort` on another field the ranks follow that
field. An empty array's key is kept apart from a null's. The key passes from
one such window to the next, and `explain` counts the key the ranks hold:
the empty key with no `$sort`, 40 bytes more than an int's per rank function.

#### Fixed

- `secantus-core` `windowfields`: the presorted stage takes each document's
  sort key off on entry, ranks by it (`RankKeys::Metadata`), sizes the
  partition cache and the rank functions by it (`CacheSize::Metadata`), and
  puts it back on the way out.
- `secantus-commands` `agg_meta`: a ranking `$_internalSetWindowFields` reads
  the sort key (`reads_sort_key`) and passes it on. The hidden key keeps an
  empty array's `undefined`, and `{$meta: "sortKey"}` reads it through
  `$ifNull` so its answers are unchanged.
  Fixes RM-410 (tests/backlog/test_rm_found_2026_11_08_explain.py::test_an_internal_window_ranks_by_the_sort_key_it_was_given);
  regression tests tests/mongod_live/test_internal_window_sort_key.py.
- Filed while measuring, each with a gap test in
  tests/backlog/test_rm_found_2026_11_10_rm410.py: RM-520 (the key lost at
  `$replaceRoot`, `$lookup`, `$setWindowFields`), RM-521 (a `$lookup`
  sub-pipeline's key), RM-522 (no key at all: 9973200), RM-523 (which `$sort`
  sets the key), RM-524 (a `$sort` before another still orders ties), RM-525
  (`{$meta: "sortKey"}` of an empty array).
