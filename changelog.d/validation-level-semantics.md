### `validationLevel` finally does something

A collection can tell the server how strictly to apply its validator, and
SecantusDB recorded the answer and then ignored it. `validationLevel: "off"`
— an explicit request for no validation at all — still had every write
checked. `"moderate"` behaved like `"strict"`, which defeats the reason the
level exists: it lets you attach a validator to a collection that already
holds rows predating it, without freezing those rows. Under our behaviour
those legacy documents became un-updatable.

Both levels now work, on both servers. `off` disables validation outright.
`moderate` exempts a document that ALREADY failed the validator from
update-time checks, while a document that currently satisfies it is still
held to it — so an update can no longer turn a valid document invalid, and
inserts are validated as before.

#### Fixed

- `validationLevel: "off"` disables document validation on the Python and
  Rust servers.
- `validationLevel: "moderate"` exempts already-invalid documents from
  update-time validation on both servers, on the single-document and
  multi-document update paths and through `findAndModify`.
