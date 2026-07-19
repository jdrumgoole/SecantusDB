### The Rust server thins aggregation input before decoding it

The aggregation pipeline fetched its input, then decoded **every** document
into an owned `bson::Document` before the pipeline ran — even documents a
leading `$skip` / `$limit` / `$match` was about to drop. A pipeline that
narrows its input early (a limit-then-group, a sample, a second filter after
the lifted one) paid to materialize rows it never used.

The leading pass-through prefix of a pipeline — `$skip`, `$limit`, and a
non-leading `$match` — now runs over the raw BSON before anything is decoded
(`$match` via the same `query::matches_raw` `find` uses), so only the survivors
that actually reach the first heavier stage (`$group` / `$sort` / computed
`$project` / `$unwind` / …) are decoded. On a 5000-row scan of wide documents
feeding `[{$limit: 50}, {$group: …}]`, that measured ~4× faster (it decoded 50 rows instead of 5000). The result is
identical to decoding everything and running the same stages — the prefix is
order-preserving and reuses the parity-pinned matcher — and any stage the
prefix doesn't handle (or a `$match` filter the raw matcher defers on) flows
through the full decode-and-run path unchanged. The heavier stages themselves
still materialize; accelerating those is separate, larger work.

#### Changed

- Rust server: an aggregation pipeline's leading `$skip` / `$limit` / `$match`
  prefix is applied over raw BSON before `decode_docs`, so the heavier stages
  decode only the documents that survive the prefix.
