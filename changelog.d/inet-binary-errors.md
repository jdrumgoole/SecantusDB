### Binary inet parameters reject malformed payloads like PostgreSQL

A malformed binary `inet` parameter now raises PostgreSQL's error classes
instead of leaking an internal XX000: a truncated header is 08P01
(insufficient data), and a bad address family or address length is 22P03
(invalid binary representation), matching `inet_recv`. The pgtest `inet`
corpus file pins all four shapes and is now green.

#### Fixed
- Empty / truncated / bad-family binary inet parameters surfaced XX000.
