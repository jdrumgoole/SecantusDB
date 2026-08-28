### A `$meta` projection no longer discards the document

Asking for a metadata field — `find({}, {score: {$meta: "recordId"}})` — came
back with just `_id`. Every other field was gone. The projection was being
treated as an inclusion list, so naming one metadata field silently excluded
everything else.

MongoDB treats `$meta` the way it treats `$slice`: as something that reshapes a
value, not as an instruction about which fields to keep. A projection
containing only `$meta` returns the whole document; combining it with a real
inclusion or exclusion field lets that field decide, and `_id: 0` still drops
the identifier.

The metadata value itself is still not computed, so the requested field is
absent from the result — but the document it was asked about now comes back
intact.

#### Fixed

- A `$meta` projection returns the document instead of reducing it to `_id`.
  `{_id: 0}`, and combining `$meta` with an ordinary inclusion or exclusion
  field, all behave as MongoDB does.
