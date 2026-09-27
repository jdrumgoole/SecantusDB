### A LIKE pattern ending in an escape character matched instead of nothing

A trailing escape has no character to escape. Falling through to the default
branch treated it as a literal backslash, so a pattern of `a` + backslash
matched a value of `a` + backslash — where PostgreSQL returns no rows.

Measured on PostgreSQL 14 (2026-09-25) with a matching row present and the
pattern bound as a parameter: no rows, and no error.

#### Fixed

- `_like_to_regex` returns a never-matching regex when the escape character is
  the last character of the pattern.

The check happens **during** the pattern scan, not with `endswith`. A pattern of
`a` + two backslashes also *ends* with the escape character, but there the first
consumes the second and it matches one literal backslash perfectly well — a
first version of this guard used `endswith` and broke exactly that case. Both
sides are pinned by the tests, along with the same pair under a custom `ESCAPE`
character and the `ESCAPE ''` case where escaping is disabled and a trailing
backslash really is literal.

Found from pgjdbc's `DatabaseMetaDataTest::escaping`.
