### A back reference works in every regular expression

`select 'abab' ~ '(ab)\1'`, `regexp_replace`, `regexp_match`,
`regexp_matches`, `regexp_count`, `regexp_instr`, `regexp_substr`,
`regexp_like` and `substring(... from ...)` now run a pattern with a back
reference, as PostgreSQL does. They compiled with the linear `regex` crate
alone, which refuses one, so each was an invalid regular expression; a
WHERE over a column already matched through `secantus-core`'s fallback.

#### Fixed

- New `pg_regex::PgRegex`: the linear engine first, and for a pattern it
  refuses the backtracking `fancy-regex`, used by the scalar operators and
  the regex builtins.
- Under `~*` a back reference still compares case-sensitively (fancy-regex
  0.14); that stays open as RP-1022.
- Fixes RP-1022 (tests/backlog/test_rp_found_rp423.py::test_a_back_reference_outside_a_where)
