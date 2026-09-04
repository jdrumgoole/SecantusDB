### jsonb_path_query returned one row; to_number returned NULL

#### Fixed

- `jsonb_path_query` is set-returning and was not registered as such, so a path
  matching many values produced a single row —
  `SELECT count(*) FROM jsonb_path_query('{"a":[1,2,3]}', '$.a[*]')` answered
  1 where PostgreSQL answers 3. Rows were silently missing, not values wrong.
- A jsonpath predicate can now be used as a whole path expression, so
  `jsonb_path_match(j, 'exists($.a)')` works. Note the rule this exposes:
  a predicate path *yields one boolean item*, so
  `jsonb_path_exists(doc, 'exists($.zz)')` is true even when `$.zz` is absent,
  while `jsonb_path_query` of the same path returns `false`.
- `to_number(text, format)` answered NULL for every input. sqlglot gives it a
  dedicated node rather than an anonymous call, so the name-keyed dispatch
  never saw it. It now parses under the format mask: decoration (`,` `G` `L`
  `$` `%`) is dropped, the sign may lead, trail or be angle brackets, excess
  decimals are truncated rather than rounded, and input with no digits raises
  `22P02`.
