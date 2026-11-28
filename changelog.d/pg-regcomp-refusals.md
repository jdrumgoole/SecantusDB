### The PostgreSQL server refuses an invalid regex as PostgreSQL does

An invalid regular expression on the Rust PostgreSQL server used to come
back as 22P02 with the message of the Rust `regex` crate (`regex parse
error: ...`), and some invalid ones were accepted: `'a' ~ 'a{2,1}'` was
simply false. PostgreSQL 15 refuses each one with 2201B and its own text
(`invalid regular expression: parentheses () not balanced`, `invalid
repetition count(s)`, `invalid embedded option`, ...). The server now runs a
port of PostgreSQL's regex parser (`regcomp.c` over `regc_lex.c`) before
the engine that matches, so it refuses the same regexes, with the same
code and message, in the same order of checks.

The check runs wherever a regex is compiled: the `~` family, `SIMILAR TO`,
`substring(... from ...)`, every `regexp_*` function under its options
(with `parse_re_flags`'s reading of them, `q` with `x` included), and a
jsonpath `like_regex` as the path is read. A table's WHERE refuses one at
plan time, before a row is read and under EXPLAIN, as PostgreSQL's planner
does when it compiles the regex for its fixed prefix.

#### Fixed

- `secantus-pgplan`: `regcomp.rs`, PostgreSQL's regex lexer and parser,
  less the NFA they build, checks a regex before the engine compiles it,
  and returns 2201B with `pg_regerror`'s text. It matched PostgreSQL 15.20
  on 28,000 random patterns under the operator and `regexp_replace`'s
  options. `regexp_count` / `regexp_instr` / `regexp_substr` now check
  their numbers before their options and their regex, as PostgreSQL does.
  Fixes RP-1123 (tests/backlog/test_rp_found_rp423.py::test_invalid_regexes,
  ::test_invalid_regexes_elsewhere, ::test_an_invalid_like_regex,
  ::test_an_invalid_regex_in_a_where_is_refused_at_plan_time).
