### timetz accepts sub-minute zone offsets

A `timetz` value can now carry a zone offset with a seconds component —
`'00:00:00+01:01:03'::timetz` parses and round-trips as `00:00:00+01:01:03`
rather than being rejected with `22007`. Postgres uses sub-minute offsets for
historical LMT zones, and the offset is preserved (and only trailing all-zero
groups are dropped, so `+01:00` still renders `+01`). This greens the pgtest
`timezone` corpus file — its remaining behaviour (session-TimeZone-aware
timestamptz rendering with historical LMT offsets, `GMT-N` upper-casing, and
binary time-type result encodings) already worked.

#### Fixed

- `datetimes.py`: the `timetz` parser accepts an `HH:MM:SS` zone offset, and the
  offset normaliser / renderer / splitter handle the sub-minute form.
