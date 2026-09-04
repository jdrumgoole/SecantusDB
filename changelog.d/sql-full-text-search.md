### Full-text search: tsvector and tsquery stop leaking as JSON

A `tsvector` and a `tsquery` are dictionaries internally, so every path that
did not know their type treated them as JSON and sent the internal
representation to the client.

#### Fixed

- `tsvector::text` and `tsquery::text` render PostgreSQL's form (`'fat':2`,
  `'fat' & 'cat'`) instead of `{"tsvector": {"fat": [2]}}`.
- `length(tsvector)` counts distinct lexemes. It was measuring the internal
  dictionary's JSON — 45 for a two-lexeme vector.
- `tsvector || tsvector` concatenates. It fell into the hstore merge branch and
  returned only the right operand, silently dropping half the document; the
  second operand's positions are now shifted past the first's, as PostgreSQL
  does, so phrase queries over the result stay correct.
- `&&` on two tsqueries is tsquery AND rather than array overlap, and
  `tsquery || tsquery` is OR.
- `to_tsvector('simple', ...)` honours its configuration and keeps stop-words.
  It dropped them under every configuration, losing tokens the caller had
  explicitly asked to index.

#### Added

- `strip`, `numnode`, `querytree`, `tsvector_to_array`, `array_to_tsvector`.
  `querytree` returns `T` for a query with no positive term, as PostgreSQL
  does.
