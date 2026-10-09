### An ordered insert on the Rust MongoDB server keeps the documents before a validation failure

When a document in an ordered `insert` batch failed the collection's
validator, the Rust MongoDB server inserted none of the batch and reported
`n: 0`. `mongod` 8.2.11 inserts the documents before the failing one and
stops there. A three-document batch whose second document fails now leaves
the first in the collection, as on `mongod`.

That was the one data difference in a probe of document validation against
`mongod` (`tools/probes/validators.py`): the Rust server differed on 15 of 68
results and now differs on none. Error 121's `errInfo` already matched.

#### Fixed

- An ordered `insert` stops at the document that fails validation (or has a
  `$`-prefixed key in its `_id`) and keeps the documents before it. Write
  errors are reported in batch order.
- `create` and `collMod` refuse a `validationLevel` or `validationAction`
  that is not one of `mongod`'s words. They were stored, and then read as the
  default.
- A validator holding `$where`, `$text`, `$near`, `$nearSphere` or `$geoNear`
  is refused.
- `collMod` of any validation option leaves `listCollections` showing both
  `validationLevel` and `validationAction`; an empty validator removes the
  validator and is not listed.
- A validation failure raised by `$out` or `$merge` carries `errInfo` and
  `mongod`'s message.
