### Plain json echoes the client's bytes verbatim; jsonb params validated

The pgtest corpus' json files pinned four fidelities. PG's plain `json`
preserves input text byte-for-byte — `SELECT $1::JSON` returns exactly
what the client sent, spacing and all, where jsonb normalises. Casts of
recoverable text (a string literal, or a json parameter's substituted
form) now validate the JSON parses and then carry the client's own text
through to the wire, for scalars and for `JSON[]` array elements alike.
This narrows the previously-documented verbatim-json divergence to
table-stored json columns only (their parsed storage shape is what
powers `->>` filter pushdown); parameter echoes and literals now
round-trip byte-exact like real PG.

Binary jsonb parameters are validated like PG: an empty payload (no
version byte) or an unknown version number is `08P01`, and invalid
UTF-8 inside is `22021` — all previously accepted silently. And a
binary array parameter whose embedded element oid names a KNOWN type
that disagrees with the declared array type (a jsonb[] payload bound as
json[]) is PG's `42804` datatype mismatch, while a garbage element oid
stays the structural `08P01`.

The `json` and `json_array` corpus files are fully green —
`json_array`'s expected-divergence entry is removed because the
divergence no longer exists.

#### Fixed

- `sql/scalar.py` / `sql/typemap.py`: verbatim `JsonText` carry-through
  for plain-JSON casts, scalar and array-element.
- `sql/pgextended.py`: jsonb binary version/empty/UTF-8 validation;
  known-type element-oid mismatch → 42804.
- `pgtest_validation/include_paths.py`: `json_array` expected-divergence
  entry removed.
