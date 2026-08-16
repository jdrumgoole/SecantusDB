### citext gets a real wire oid

citext columns and parameters now report oid 90008 — the stable placeholder
CockroachDB uses for the extension type, mirroring how hstore rides 16935 —
instead of collapsing into text's oid 25. ParameterDescription infers it for
INSERT targets and for unknown parameters compared against a citext column
(citext only: it ships its own operator family, unlike types that resolve
comparisons through text), RowDescription reports it for citext columns, and
binary parameter/result formats carry the text bytes. Case-insensitive
matching is unchanged. The pgtest `citext` corpus file pins the whole
exchange byte-for-byte and is now green.

#### Changed
- citext's wire oid: 25 → 90008 (drivers treat the unknown oid as text, so
  text-mode round-trips are unaffected).

#### Added
- Binary param/result codecs for citext (raw text bytes).
- Comparison-against-citext-column parameter inference (oid 90008).
