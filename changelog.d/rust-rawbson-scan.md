### The Rust server stops decoding whole documents just to filter them

Phase 2 of the raw-BSON serving-path work takes the scan path off owned-BSON
materialization. The Rust server's collection scan used to decode every
candidate document into an owned `bson::Document` — a heap allocation per
field — purely to run the query filter over it, then throw that document
away for any document the filter rejected. For a selective filter over wide
documents that is almost all wasted work: the profiler put this scan-side
materialization (with the reply-path decode fixed in the previous release)
at the larger share of the serving path's on-CPU time.

The filter now runs over the raw BSON bytes. A new
`secantus_core::query::matches_raw` walks the document by field name and
decodes **only the fields the filter actually reaches** — a filter on one
field of a ten-field document never touches the other nine — reusing every
existing operator unchanged. Documents the filter rejects are never fully
decoded, and a filter-only find (no in-memory sort) decodes nothing at all;
only the matched documents of a post-sorted find are decoded, for their
sort keys. The raw matcher is pinned bool-for-bool to the owned matcher (and
transitively to the pure-Python matcher) across the entire curated + fuzz +
regex + collation parity corpus, so no query answers change.

A selective filter over wide documents that this optimizes — a `count` or
`find` that scans many rows and keeps few — measured **~2.8× faster** on a
5000-row collection scan (one filter field of eleven, rejecting 99.8%).

#### Changed

- Rust server: `find` and `count` scan filtering runs over raw BSON
  (`query::matches_raw`) instead of materialising each candidate into an
  owned `Document`. Selective filters over wide documents skip decoding the
  fields they don't touch; rejected documents and no-sort finds skip the
  owned-document build entirely. (The update/delete candidate scans still
  materialise, since a matched document is needed for the write — a later
  phase.)

#### Added

- `secantus_core::query::matches_raw(&RawDocument, …)` — the raw-BSON query
  matcher, exposed to the parity harness as `_secantus_core.query_matches_raw`
  and cross-checked against `query_matches` on every parity case.
