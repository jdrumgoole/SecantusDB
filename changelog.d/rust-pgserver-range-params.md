### Ranges as parameters, and a bug that only appeared when one was

Range types shipped in the previous release, and they worked — as long as every
part of the range was written into the query. Bind any of it as a parameter and
the whole thing fell over, with an error quoting a fragment of this server's own
internal text back at the client.

The cause is a sequencing detail of the wire protocol. A client may ask the
server to *describe* a statement before it supplies any values, so at that point
every parameter is null. A null bounds argument is genuinely an error in
PostgreSQL — `int4range(1, 5, null)` is rejected — and treating it as one while
describing meant every `int4range(%s, %s, %s)` failed before it was ever given
values. The two cases have to be told apart by where the null came from: written
in the query, it is an error; still unbound, it is not.

Ranges can now also be sent as values in either wire format, which for the binary
one means reading each bound in the *element* type's own binary form rather than
anything range-specific.

One further gap turned up underneath. A `tsrange` has to order its own bounds to
canonicalise, and a timestamp carrying sub-millisecond digits is stored in a
composite form that had no comparison at all — so the failure reported comparing
range bounds when the missing piece was really timestamp comparison. Two
timestamps now compare as the instants they are, however they are stored.

#### Fixed

- A range constructor with a bound parameter failed at describe time, before any
  value was supplied.
- Ranges could not be sent as parameters in either wire format.
- Two timestamps carrying sub-millisecond digits could not be compared, which
  surfaced as a range error.
