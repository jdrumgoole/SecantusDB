### Full-text search now stems

`to_tsvector('english', …)` did not stem, so `cats` did not match `cat` and a
search for `quick` did not find a row whose title is `Running quickly`. That is
the worst class of search defect: a query that should match returns nothing,
with no error.

#### Added

- `secantus.sql.snowball` implements the English (Porter2) algorithm
  PostgreSQL's `english` configuration uses. It is written out rather than
  taken from a dependency, since SecantusDB ships self-contained wheels and a
  stemmer is a closed, fully specified algorithm. It is pinned against **6,094
  words stemmed by PostgreSQL itself** (`tests/data/english_stems.txt`), and
  matches on every one.

#### Fixed

- Documents and queries both stem, which is the point — a query's `running` and
  a document's `runs` meet at `run`. Prefixes stem too, so `running:*` renders
  as `'run':*`.
- `to_tsquery` drops stop-words, as the document side always did. Keeping them
  produced a query that could never match, since no document indexes them.
- `simple` still neither stems nor drops stop-words.
