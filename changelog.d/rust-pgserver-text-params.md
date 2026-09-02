### A parameter should not mean different things in different wire formats

A client may send each parameter as text or in PostgreSQL's binary format, and
picks per value — psycopg sends most things binary and falls back to text. The
two are decoded by separate code here, and only the binary side had learned
arrays, intervals and timestamps. Sent as text, those values fell through to a
plain string, so `array[...] = %s` compared an array against a string and
reported that it could not compare them. The message pointed at comparison; the
cause was one layer earlier, in decoding.

Both formats now produce the same value for the same declared type, and the
mapping from an array type to its element type is shared between them, so they
cannot drift apart again.

The other half is subtler. A client may leave a parameter's type *unspecified*
and let the server work it out — psycopg does this for lists and datetimes — and
PostgreSQL then resolves it from whatever it is being compared to, exactly as it
resolves an unquoted literal. That rule was already implemented for literals;
extending it to parameters is what makes `'2026-01-01 12:00'::timestamp = %s`
answer true rather than complain about comparing a timestamp to a string.

#### Fixed

- Array, interval, date, time and timestamp parameters sent in the text format
  decoded to plain strings, so comparing one to a value of its own type failed.
- A parameter whose type the client left unspecified was not resolved from the
  operand beside it, though a literal in the same position was.
