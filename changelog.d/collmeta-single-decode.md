### Write ops decode the collection-options row once, not three times

Every insert decoded the collection-options blob twice (the timeseries
check, then the UUID fetch for the oplog entry), and every replace/delete
decoded it twice more (UUID, then the pre/post-image flag) — the same tiny
BSON row, searched and decoded repeatedly within one operation. A one-decode
`CollMeta` view now feeds all three consumers; the collection UUID stays
lazily minted only when the oplog actually needs it, so a server running
with the oplog disabled mints exactly as few UUIDs as before. Measured
paired A/B on batch inserts into a two-index collection: +2.3% (5/5 positive
pairs).

#### Changed
- `secantus-storage`: `coll_meta` / `meta_uuid` replace the per-op
  `is_timeseries` + `collection_uuid` + `pre_post_images_enabled` call
  chains on the insert/replace/delete paths. Behaviour is unchanged —
  same facts, one decode.
