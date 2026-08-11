### Document validation you can actually stage, and an admin UI that reaches the rest of the server

Setting `validationAction: "warn"` on a collection is how you stage a
validator against live traffic — mongod logs the violations and stores the
document anyway. The Python server accepted the option, reported success,
and then rejected the write with code 121 regardless, so the one workflow
the setting exists for was the one it broke. `collMod` had the same shape
of problem from the other end: it replied `ok: 1` to `validationAction`
and `validationLevel` and quietly discarded both, leaving callers
convinced they had relaxed enforcement that was still fully armed. Both
are fixed, on every write path, and the Rust server — which already got
this right — is now matched exactly.

The admin UI also stopped hiding features the server has shipped for a
while. Collections can be created with validators and capped options,
modified with `collMod`, and renamed (across databases, with an optional
`dropTarget`); custom roles can be created and dropped. The change-stream
page gained the options that make it a real debugging tool: `fullDocument`
and `fullDocumentBeforeChange`, all three start points, and a pipeline
filter — plus a **Resume from here** button on every event, which finally
closes a loop the page had left open by offering a "Copy resume token"
button with nowhere to paste the token.

#### Added

- Admin: create / `collMod` / rename panels on the collection list, and
  create / drop for custom roles on `/roles`. Options are entered as one
  Extended-JSON document, so any option the target server understands
  works without waiting for a matching form field.
- Admin: `fullDocument`, `fullDocumentBeforeChange`, `resumeAfter`,
  `startAfter`, `startAtOperationTime` and pipeline controls on the
  change-stream page, with a **Resume from here** action per event.
  Options round-trip through the URL, so a shared link reproduces the
  same stream.

#### Fixed

- `validationAction: "warn"` and `"off"` now accept violating writes
  instead of rejecting them with `DocumentValidationFailure` (121), on
  insert, update, replace and `findAndModify` alike. Only the default
  `"error"` rejects.
- `collMod` now applies `validationAction` and `validationLevel` rather
  than accepting and discarding them.
- Admin: a rejected change-stream option is reported as a readable error
  frame instead of a bare websocket close, and the message is no longer
  overwritten by the disconnect handler that followed it.
