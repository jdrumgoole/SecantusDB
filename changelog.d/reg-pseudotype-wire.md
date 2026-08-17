### reg* pseudo-types on the wire

The registry pseudo-types — regclass, regtype, regproc, regprocedure,
regnamespace, regrole — now use PostgreSQL's oid wire representation: a
4-byte unsigned integer in binary format, and DataTypeSize 4 in
RowDescription. Previously a binary parameter of one of these types passed
its raw bytes through untouched, so a client sending the standard 4-byte
form got the bytes back instead of the numeric value. A payload of any
other length is rejected with 08P01, matching PG's `oidrecv`. The pgtest
`oid` corpus file pins all of it and is now green.

#### Fixed
- Binary reg* / oid parameters echoed raw bytes instead of decoding.
- reg* columns reported variable width instead of typlen 4.
