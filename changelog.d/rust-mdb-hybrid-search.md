### Rust MongoDB server: `$score`, `$rankFusion` and `$scoreFusion`

The three hybrid-search stages mongod 8.2 added now run on the Rust MongoDB
server. Each is desugared as mongod 8.2.11 desugars it (its `explain` shows
the stages), with the score kept beside the document and read back through
`{$meta: "score"}` and `{$meta: "scoreDetails"}`. Measured against mongod
8.2.11 over 84 pipelines (values, order, `scoreDetails` and errors), with no
difference.

#### Added

- `$score`: the score of an expression, `normalization: none | sigmoid |
  minMaxScaler`, `weight` from 0 to 1, `scoreDetails`.
- `$rankFusion`: reciprocal rank fusion of ranked input pipelines, `weight /
  (60 + rank)` summed per document. Documents that tie on the input's sort
  key share a rank, as with mongod's `$rank`.
- `$scoreFusion`: scored input pipelines combined by `average` or a custom
  `expression`, after `none`, `sigmoid` or `minMaxScaler` normalisation.
- Both fusions sort by score descending and `_id` ascending. A document
  missing from an input pipeline scores 0 there.
- The score metadata survives every stage that keeps the document,
  `$replaceRoot` and `$lookup` included, and ends at `$group`. A read with
  no score in reach is mongod's 40218.
- At FCV 8.0 each stage is refused with mongod's 224 and its own text.
- Fixes RM-198 (`tests/backlog/test_rm_added_in_82.py::test_hybrid_search_stages_82_added`,
  `crates/secantus-storage-adapter/tests/hybrid_search_wt.rs`).

#### Not yet

- `explain` shows the stage as written rather than mongod's desugaring, and
  a fusion stage at the head of a `$lookup` or `$facet` sub-pipeline is
  refused (RM-200).
