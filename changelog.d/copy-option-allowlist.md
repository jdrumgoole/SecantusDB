### COPY option validation and custom CSV quote characters

COPY now validates its option keywords against PostgreSQL's grammar: an
unknown keyword (for example CockroachDB's `WITH destination = '…'` upload
extension) raises PG's 42601 syntax error at parse time, before the target
table is resolved — previously it fell through to a misleading 42P01. The
`QUOTE` option is also implemented for real: a custom CSV quote character
applies to both COPY FROM parsing and COPY TO rendering, is rejected outside
CSV mode with PG's 0A000, and a multi-character quote raises 22023. The
pgtest `copy_file_upload` corpus file pins the 42601 shape and is now green.

#### Added
- `COPY … CSV QUOTE 'x'` — custom quote characters in both directions;
  `ENCODING`/`FREEZE`/`OIDS` accepted as no-ops.

#### Fixed
- Unknown COPY option keywords raise 42601 (syntax error), not 42P01.
