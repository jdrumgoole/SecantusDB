### Rust pgserver: multirange arrays report their element type

An `ARRAY` of multiranges was typed as `varchar` on the Rust PostgreSQL server,
where an array of ranges already carried its real element type. That broke the
value in BOTH wire formats: in text the client read back a bare string like
`{{[1,5)},{}}` instead of parsing it into `Multirange` objects, and in binary a
`varchar` column stays on the binary path — where an array value cannot be sent
as a binary `varchar` at all (`22P03`). The six multirange array types now
report their own array oids (`int4multirange[]` is 6150, and so on), which keeps
them, like range arrays, on the text-format path the row description already
downgrades a non-binary-encodable type onto — so the client parses them.

The scalar range/multirange binary wire codec was already correct: typed empty,
unbounded and populated range/multirange values round-trip in binary against a
real server; the only remaining range gaps are `CREATE TYPE ... AS RANGE`
(custom range types) and array-of-range element comparison, both separate work.

#### Fixed
- `ARRAY`-of-multirange results now report the multirange's array type
  (`int4multirange[]` = 6150, `int8multirange[]` = 6157, `nummultirange[]` =
  6151, `datemultirange[]` = 6155, `tsmultirange[]` = 6152, `tstzmultirange[]` =
  6153) instead of `varchar`, in both wire formats.
- Binary parameters of those array oids decode through the multirange element
  decoder.
