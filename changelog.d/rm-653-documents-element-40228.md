### A `$documents` element that is not a document is mongod's 40228

`$documents: [{a: 1}, 2]` now fails as mongod 8.2.11 does: at run time with
40228 "elements within the array passed to $documents must evaluate to an
object, but resulting value was: 2. Type of resulting value: 'int'. Input
document: {_tempDocumentsField: 2}", under the executor prefix naming the
command's namespace (the outer collection inside a `$lookup`).

#### Fixed

- The Rust MongoDB server answered 2 BadValue "each $documents element must
  be an object".

Fixes RM-653 (tests/backlog/test_rm_found_2026_11_14_rm531.py::test_a_documents_element_that_is_not_a_document_is_40228)
