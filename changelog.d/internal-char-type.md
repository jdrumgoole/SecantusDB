### PG's internal one-byte "char" type

The quoted `"char"` spelling now names PostgreSQL's internal one-byte type
(oid 18, typlen 1) instead of collapsing into `char(n)`/text: casts report a
column named `char` with oid 18, table columns declared `"char"` describe and
bind parameters with oid 18, input values truncate to one character, an
empty string or zero byte stores SQL NULL, `0::"char"` produces the zero
byte (rendered as one `0x00` byte in binary result format), and binary
parameter/result codecs carry the raw byte. sqlglot loses the quoting — the
quoted and unquoted spellings both parse as plain CHAR — so the planner
rewrites the quoted form to an internal sentinel type name before parse,
token-context aware: it fires after `::`, after `AS` only inside `CAST(...)`,
and in a CREATE/ALTER column-type position, never on aliases, column names,
or string literals. The pgtest `char` corpus file pins the whole surface
byte-for-byte (its one remaining stanza expects CockroachDB's deterministic
TableOID and cannot pass against any non-crdb server; recorded as an
expected divergence).

#### Added
- `"char"` (quoted, oid 18) as a first-class column and cast type: 1-char
  truncation, NULL for empty/zero-byte input, `int::"char"` as chr(i) with
  22003 out-of-range, binary param/result wire codecs, typlen 1 in
  RowDescription.

#### Fixed
- `SELECT 'a'::"char"` reported oid 25 with a `bpchar` column name; it now
  reports oid 18, size 1, named `char` (pgtest `char:42`).
