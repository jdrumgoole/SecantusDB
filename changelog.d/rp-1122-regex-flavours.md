### Rust PG server: a regex's director, embedded options and newline default are PostgreSQL's

A regex now compiles the way PostgreSQL's `regcomp` reads it. A `***=` or
`***:` director, an ARE's embedded options (`(?q)`, `(?b)`, `(?e)`, `(?c)`,
`(?n)`, `(?p)`, `(?w)`, ...) and a regex function's options argument all
choose the regex's flavour and flags. By default the regex is not
newline-sensitive, so `.` and a negated bracket match a newline. This holds
for the `~` family, the regex functions, `SIMILAR TO`, `substring`, a
table's WHERE, ident maps and ispell affixes.

#### Fixed

- `select 'a.c' ~ '(?q)a.c'`, `'^w' ~ '***=^w'`, `'w0+' ~ '(?b)w0+'`,
  `'wd' ~ '(?e)w\d'` and `E'x\nab' ~ '(?n)^ab'` were 22P02 "unrecognized
  flag" (or "repetition operator missing expression"); each is `t`, as on
  PostgreSQL 15.20, and `'W' ~* '(?c)w'` is `f`.
- `select E'a\nb' ~ 'a.b'` was `f` and `regexp_replace(E'a\nb', 'a.b',
  'X')` left the text alone; a `.` matches a newline unless `n`, `m` or `p`
  makes the regex newline-sensitive, and under those a negated bracket
  does not match a newline.
- A regex function's `e` option gives a BRE, as `parse_re_flags` makes it
  (`regexp_match('w(0)', 'w(0)', 'e')` is `{w(0)}`), and its options are
  read left to right (`'ic'` is case-sensitive).
- An escaped character that is no letter or digit is the character itself:
  `'a<b' ~ 'a\<b'` is `t` (the `regex` crate reads `\<` as a word bound).

Fixes RP-1122 (tests/backlog/test_rp_found_rp423.py::test_regex_flavours)
