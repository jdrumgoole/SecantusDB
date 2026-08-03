### BC timestamps, and parameters that kept their declared type

A date before year 1 stored in a `timestamp without time zone` column came back
carrying a time-zone offset it should never have had — `0101-01-01 00:00:00+00
BC` where Postgres writes `0101-01-01 00:00:00 BC`. Ordinary dates already
dropped the offset; only the ones outside the range Python can represent kept
it.

Separately, a parameter the client declared as `timestamp with time zone` lost
that declaration on the way to the column. Stored into a `timestamp` column it
was treated as though it had been typed out as a literal — offset discarded,
clock face kept — instead of being converted through the connection's zone, so
the value moved by the zone's offset. A client in New York writing midnight got
five in the morning back.

#### Fixed

- A BC or far-future timestamp in a `timestamp without time zone` column no
  longer reports an offset.
- A `timestamp with time zone` parameter keeps its type when stored into a
  `timestamp` column, and converts through the session's zone.
