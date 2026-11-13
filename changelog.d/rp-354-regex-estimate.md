### EXPLAIN estimates a regular-expression match

A filter that matches a column against a regular expression (`~`, `~*`,
`!~`, `!~*`) used to print `cost=0.00..0.00 rows=0` in EXPLAIN, on a lone
scan and in a LEFT join's ON condition alike. It is now estimated the way
PostgreSQL 15 estimates it. The fixed prefix comes from the anchored start
of the regex, read as PostgreSQL's regex compiler reads it: an exact count
repeats a character, `+` keeps one copy, and an optional or ranged atom,
a bracket of several characters or a case-insensitive letter stops the
prefix. The rest of the pattern gets PostgreSQL's per-character guesses.
The column's most common values and histogram entries that the regex
matches make up the rest of the estimate. A regex whose prefix cannot be
read that way, such as a repeated group or a back reference, still prints
zeros rather than a guess.

#### Fixed

- EXPLAIN of a scan or a nested loop filtered by a regex match over an
  analysed text column prints PostgreSQL's rows and costs, where it printed
  zeros (`regex_fixed_prefix`, `regex_selectivity` and `patternsel` for
  the regex operators).

Fixes RP-354 (tests/backlog/test_rp_explain_costs.py::test_plan[a left join's on conjunct over the preserved side is a regex match]); also the regex cases of RP-423 (tests/backlog/test_rp_explain_costs.py::test_plan[estimate of an anchored regex match], tests/backlog/test_rp_explain_costs.py::test_plan[estimate of an unanchored regex match]).
