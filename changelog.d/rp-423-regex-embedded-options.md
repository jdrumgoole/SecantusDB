### EXPLAIN estimates a regex with a director or embedded options

A PostgreSQL regular expression can open with a director (`***:`, `***=`)
or embedded options (`(?x)`, `(?i)`, `(?q)`, `(?b)`, `(?n)` and the rest),
which change how the rest of it is read: expanded syntax drops white space
and comments, `(?q)` makes it a literal string, `(?b)` and `(?e)` make it a
basic or extended regex, and `n`, `m` and `w` let `^` match after a
newline. The Rust PostgreSQL server's planner now reads them as
PostgreSQL's `regcomp` does before it looks for the regex's fixed prefix,
and matches the histogram under the same flags, so EXPLAIN prints
PostgreSQL 15.20's estimate for such a filter instead of zeros.

#### Fixed

- EXPLAIN estimates a regex match with a director or embedded options:
  `(?x)^w 0 0` by its prefix `w00`, `(?q)^w00` and `***=^w00` as a literal
  string with no anchor, `(?b)^w0+` as the basic regex whose prefix is
  `w0+`, and `(?n)^w00` with no prefix. An ARE's `(?#...)` comments are
  dropped, and a bracket holding an escaped character or a class escape
  (`[\]]`, `[\d]`) is read as PostgreSQL reads it.
  Fixes RP-423 in part (tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a regex match with embedded options])
