### pg_get_keywords(), aggregates over SRF row sources, and <> ALL(array)

`pg_get_keywords()` joins the SRF family (word / catcode / barelabel /
catdesc / baredesc; ~100 PG-specific keywords incl. `reindex`), the
record-SRF machinery now handles any column count (it assumed two), and
aggregates over an SRF row source work — `SELECT string_agg(word, ',')
FROM pg_get_keywords()` is pgjdbc's getSQLKeywords query, previously
"not supported in this context". The planner rewrites an
aggregate-over-SRF select into the derived-subquery shape the pipeline
already handles; scalar subqueries whose FROM is an SRF route through
the engine the way ordered/grouped subqueries do; `x <> ALL(array)` /
`= ALL(array)` evaluate in scalar contexts; and a function-wrapped
`string_agg` (`decode(string_agg(…), 'hex')`) registers like the plain
form instead of erroring.

#### Added
- `pg_get_keywords()` SRF; `= ALL` / `<> ALL` over array values.

#### Fixed
- Aggregates over `FROM generate_series(…)` / other SRF sources
  (previously only `count(*)` worked).
- Scalar subqueries with an SRF FROM (`(SELECT string_agg(…) FROM
  generate_series(…))`).
- Function-wrapped `string_agg` in the computed-projection paths.
- Record SRFs with more than two columns.
