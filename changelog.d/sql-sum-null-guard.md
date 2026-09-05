### sum() answered 0 for a group that contributed nothing

`SELECT j.id, sum(k.v) FROM j LEFT JOIN k ON k.jid = j.id GROUP BY j.id`
answered `0` for a `j` row with no match, where PostgreSQL answers NULL — a
common reporting query, silently wrong, and one a caller could not defend
against, since `coalesce(sum(...), -1)` also saw the 0.

#### Fixed

- The NULL guard on `sum` counted contributions with a bare
  `$ne: [value, null]`, which is **true for a missing field**. An unmatched
  outer-join row carries no key at all for the non-driving side, so the guard
  counted a contribution that never happened. It now collapses missing into
  null first, exactly as `COUNT(col)` does. This is why only the unmatched-row
  case was wrong while a group holding a genuinely NULL value was already
  right.
- `HAVING sum(x) IS NULL` could never be true — neither the single-table nor
  the join `HAVING` path applied the guard at all. Both now do.
