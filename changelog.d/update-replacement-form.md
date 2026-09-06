### The first key decides whether an update is operators or a replacement

`{z: 2, $set: {a: 1}}` is not an operator update. mongod reads the **first key
alone** to decide an update's form, so that document is a *replacement* that
happens to contain a `$`-prefixed field — which it refuses with
`DollarPrefixedFieldName` (52) and a message pointing at `$replaceWith`. Both
servers instead asked "does any key start with `$`", called it an operator
update, and answered `9 Unknown modifier: z`. Reverse the two keys and mongod
agrees with the old answer, which is what made the difference easy to miss.

The two errors are not just worded differently; they fire at different times,
and that is observable. The operator-form `9` is a parse error, reported even
when the filter matches nothing and on an upsert. The replacement-form `52` is
an *execution* error: with no matching document the statement is a silent no-op
(`n: 0`), and an upsert **inserts the document verbatim, `$`-key and all**. Both
servers used to reject all three cases up front, so a legitimate no-match update
failed and a legitimate upsert never happened.

Only the top level is restricted. mongod 8.x stores `{a: {$bad: 1}}`,
`{a: [{$bad: 1}]}` and even a literal dotted key `{"a.b": 1}` without
complaint, and `insert` accepts all of those too — all already correct on both
servers, and now pinned so they stay that way.

#### Fixed

- `secantus.update` / `secantus-core`: an update's form is decided by its first
  key, via one shared `is_operator_form` predicate rather than three separate
  `any(...)` tests.
- `secantus.update` / `secantus-core`: a `$`-prefixed top-level key in a
  replacement is mongod's `DollarPrefixedFieldName` (52), raised at execution
  time and carrying the `Plan executor error during <command> :: caused by ::`
  wrapper, with the first such key named. A no-match update is a silent no-op
  and an upsert inserts the document unchanged.
- `secantus.storage`: an upserted **replacement** keeps the document's own field
  order (`_id` first, then as sent) instead of being re-sorted by field name,
  which had put a `$`-prefixed key ahead of a plain one. Operator upserts are
  unaffected.
