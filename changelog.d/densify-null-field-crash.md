### `$densify` no longer crashes on a null or missing field

Running `$densify` over a collection where any document's target field was
`null` or absent returned an internal server error. The stage sorted documents
by that field without checking it existed, so Python's own comparison raised —
`'<' not supported between instances of 'NoneType' and 'int'` — and the failure
escaped as a generic "internal server error" rather than anything actionable. A
single document with a missing field was enough to take down the whole
aggregation.

MongoDB simply doesn't densify those documents: they pass through unchanged, in
their normal sort position ahead of the numbers, and the remaining values
densify as usual. A field holding something that is neither a number nor a date
is rejected outright with a specific error rather than a crash.

Both servers now behave that way. The Rust server had been declining the stage
entirely whenever such a document was present — its code carried a comment
explaining that it deferred because the Python side raised — so it inherited the
bug as an error rather than a crash. Both are fixed together.

#### Fixed

- `$densify` emits a document whose target field is `null` or missing unchanged
  instead of failing the aggregation, and densifies only the documents that have
  a usable value.
- A non-numeric, non-date value in the densify field is rejected with MongoDB's
  `5733201` (`Densify field type must be numeric or a date`) instead of an
  internal error.
