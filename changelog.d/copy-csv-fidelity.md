### COPY reaches byte-exact CSV fidelity

COPY's CSV codec now mirrors PostgreSQL's own parser: quoting decides
NULL-ness (a quoted `"N"` under `NULL 'N'` is the string N, a quoted empty
field is the empty string, and only unquoted cells match the null token),
custom `ESCAPE` characters work inside quoted fields, a `\.` line terminates
the data stream, an unterminated quoted field raises 22P04, and COPY TO CSV
force-quotes empty strings so they stay distinct from NULL. The legacy
un-parenthesized option syntax (`CSV NULL 'NS' DELIMITER '|'`) parses
correctly, ESCAPE and HEADER outside CSV mode raise PG's 0A000, and
text-format `\xHH` / octal byte escapes decode on COPY FROM. COPY (query)
TO STDOUT also works through the extended query protocol now — Describe
answers NoData and Execute streams CopyOutResponse/CopyData/CopyDone — with
PG's exact error shapes for parameters (COPY takes none: binding any is
08P01 with the statement-summary detail; an unbound placeholder at Execute
is 42P02). The pgtest `copy` corpus file (1187 lines) pins all of it and is
fully green.

#### Added
- Extended-protocol `COPY (query) TO STDOUT` (Parse/Bind/Describe/Execute).
- CSV `ESCAPE` character support and `\.` terminator handling.
- Text-format `\xHH` / `\OOO` byte-escape decoding on COPY FROM.
- crdb-style inline `INDEX (...)` table elements are accepted (index
  skipped); `ADD COLUMN ... NOT VISIBLE` parses as a normal column.

#### Fixed
- Quoted CSV cells equal to the NULL token no longer read back as NULL.
- COPY TO CSV writes empty strings as `""` (previously indistinguishable
  from NULL).
- Legacy COPY option lists (`CSV NULL 'NS' DELIMITER '|'`) no longer
  mis-parse into default delimiters.
- ESCAPE/HEADER without CSV raise 0A000 instead of silently entering copy
  mode (which deadlocked the connection).
