### Binary means binary, and a cursor is a portal

Two things a client asks for and this server quietly answered differently.

A cursor opened for binary results got its rows in text. The values were right
— the format travels per column in the row description, so the client dutifully
decoded text and handed back the right Python objects — which is exactly why it
went unnoticed: nothing was ever wrong except the thing the client asked for.
Result columns now go out in the requested format for the types this server can
render exactly in PostgreSQL's binary layout: booleans, the integer and float
widths, the string types, `numeric`, and arrays of those. A column outside that
set is still described as text, which the client reads correctly, and the gap is
written down rather than hidden.

The other is server cursors. PostgreSQL exposes a declared cursor as a portal of
the same name, and psycopg describes that portal straight after the `DECLARE` —
before it fetches anything — to learn the columns. This server's cursors were
its own, so the describe found nothing and every server cursor died on its first
row with "portal not found". They now answer for the cursor of that name.

`numeric` was the interesting half of the format work: its binary layout is
base-10000 groups aligned on the decimal point rather than on the digit string,
so `0.00001` is a single group of `1000` two places below the point, not a group
that straddles it.

#### Added

- Binary result columns for `bool`, `int2`/`int4`/`int8`, `float4`/`float8`,
  `text`/`varchar`/`bpchar`/`name`/`char`, `numeric`, and arrays of those.
- An answer for `Describe portal` naming a declared cursor, which is how
  psycopg's server cursors learn their columns.

#### Fixed

- A cursor asking for binary results received text.
- Every psycopg server cursor failed with "portal not found".
