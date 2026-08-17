### Parameter typing follows PostgreSQL's parse analysis

Prepared-statement parameters now take their types the way PostgreSQL's parse
analysis assigns them. A parameter compared or assigned against a column gets
that column's type — `UPDATE t SET ts = $1 WHERE id = $2` describes as
timestamptz and uuid rather than text — where previously only citext and ltree
columns did this. Conflicting uses are rejected instead of silently
succeeding: because a parameter has exactly one type, `SELECT lower($1), $1::int`
raises 42883 (no `lower(integer)`), a gap in the numbering (`SELECT $2 > 0`,
with no `$1`) raises 42P18, and a bare parameter as a CASE's only result
raises 42P18 too — while a CASE with a typed sibling branch still resolves.
Unaliased column names also follow PG's `FigureColname` precedence: a cast
takes its operand's name when it has one (`n::int4` is `n`), falling back to
the type name only for nameless operands (`2::int8` is `int8`). The pgtest
`parameter_description` corpus file pins all of it and is now green.

#### Added
- Column-derived parameter types for assignments and comparisons.
- 42883 / 42P18 rejections for unresolvable parameter typings.

#### Fixed
- A cast of a column reported the type name instead of the column name.
