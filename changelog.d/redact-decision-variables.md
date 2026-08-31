### `$redact` returned data it exists to withhold — three defects, both servers

`$redact` is MongoDB's content-based access-control stage: an expression decides,
per document and per sub-document, whether to keep it, drop it, or recurse. Three
separate defects made it hand back documents mongod withholds. All three were
present on **both** servers, and all three were found by running the stage against
mongod 8.2.11 rather than by reading the code — which had described the decisions
as "sentinel strings", the assumption the first two bugs rest on.

#### Fixed

- **A stored string could impersonate a decision.** The stage compared the
  evaluated result against the *string* `"$$KEEP"`, so a document whose own field
  held that string satisfied the test: `{"$redact": "$tag"}` over caller-supplied
  content kept a document — with its secrets — that mongod refuses to keep.
  `$$KEEP` / `$$PRUNE` / `$$DESCEND` are now variables bound only while
  `$redact`'s expression is evaluated, and the stage dispatches on the bound
  marker, so no value carried in a document can be mistaken for a decision.
- **Redaction skipped nested arrays.** The descent walked documents and
  arrays-of-documents but passed a *nested* array through untouched, so a
  sub-document one array deeper — `[[{level: 9}]]` — was returned in full where
  mongod prunes it and leaves the emptied inner array in place.
- **The decision names leaked outside `$redact`.** `{"$project": {"x": "$$KEEP"}}`
  returned the marker string as ordinary data. mongod binds these three names only
  inside `$redact` and answers `Use of undefined variable: KEEP` (17276) anywhere
  else, which both servers now do.
- **A non-decision result now answers mongod's error.** 17053 with its own
  wording, wrapped in `Executor error during aggregate command on namespace: … ::
  caused by ::`, and the offending value rendered mongod's compact way — the
  Python server answered a generic TypeMismatch (14) with its own text, and the
  Rust server reported the stage as unsupported.
- **An undefined variable takes the wrapper mongod gives it.** `Invalid $<stage>
  :: caused by ::` inside `$project` / `$addFields` / `$set`, and a bare message
  in every other stage; the Python server applied the executor prefix to both.

#### Notes

- `$redact` was never missing: it worked on both servers for every valid pipeline,
  which is why only its error path had been noticed. The backlog entry calling it
  "unimplemented on the Rust server" was wrong and has been corrected.
- The 17053 value rendering is mongod's compact `Value::toString` (`{k: 1}`,
  `[1, "a"]`, a bare ObjectId, an ISO-8601 date) — **a different renderer** from
  the shell form other messages use (`{ k: 1 }`, `ObjectId('…')`,
  `new Date(<ms>)`). Both are mongod's; they are not interchangeable.
- The Rust server had no executor wrapper at all. It now applies one where mongod
  does, via a standalone validator that names the error the engine could only
  signal as `Fallback` — the `update::arith_type_error` template, so `Fallback`
  itself is untouched.
- 21 cases added to `tests/test_mongod_differential.py`, so this is measured
  against a real `mongod` from now on rather than pinned to our own belief.
