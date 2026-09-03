### SQL: remove the superseded `to_char` strftime machinery, correct the docs

Follow-up to the datetime template engine. Housekeeping found by reconciling
the documentation against what the code now does.

#### Changed

- Removed 98 lines of dead `to_char` machinery that the template parser
  superseded (`_repair_time_format`, `_render_word_token`, `_PG_WORD_TOKENS`,
  `_WORD_TIME_TOKEN_RE`, `_WORD_TIME_DIRECTIVES`, `_WORD_TIME_PAD`). Nothing
  referenced it, and its comment still asserted that `IYY` / `IY` / `I` / `IDDD`
  "have no strftime directive and are not handled" — a limitation that no longer
  exists, pointing at a backlog entry that is now resolved.

#### Fixed (documentation)

- Three of the four worked `to_char` numeric examples in `docs/sql.md` showed
  pre-`FM`-fix output: `FM999,999.99` on `1234.5` is `1,234.5`, not `1,234.50`.
  All examples on the page are now verified against PostgreSQL 14.13.
- `docs/sql.md` claimed `RN` (Roman numerals) was unimplemented. It is
  implemented and matches Postgres.
- Added a `to_char` / `to_date` / `to_timestamp` **datetime** section — the new
  template engine had no user documentation.
