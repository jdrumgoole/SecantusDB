### array_agg told "no rows" and "a row with no value" apart

#### Fixed

- `array_agg` over zero contributing rows is NULL, not `{}` — which a caller
  could not recover from, since `{}` is itself a legal value. The same applies
  to `json_agg` and to a `FILTER` that matches nothing.
- An unmatched outer-join row contributes a NULL element, so
  `array_agg(k.v)` over a `LEFT JOIN` gives `{NULL}` rather than `{}`.

Both halves had to land together: `$push` of a missing field pushes nothing, so
the pushed array could not tell the two cases apart until the value was wrapped
to leave an explicit null behind. The wrap is per-aggregate — `array_agg`,
`json_agg` and `jsonb_agg` keep null elements while `string_agg` skips them and
is correct to answer NULL for a group of nothing but nulls.

- `array_agg` over a JOIN reports its element's array type, where it had
  reported `jsonb`; the same call over a single table already did.
