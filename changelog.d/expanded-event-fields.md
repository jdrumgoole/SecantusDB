### Expanded change events were missing the collection UUID

With `showExpandedEvents`, MongoDB puts `collectionUUID` on every event that has
a collection. SecantusDB set it only on inserts, updates and deletes, so a
consumer watching DDL — `create`, `createIndexes`, `dropIndexes`, `collMod`,
`drop`, `rename` — could not tell which collection a UUID-keyed event belonged
to without a second lookup.

`invalidate` is the deliberate exception: it derives from an event that does
carry a UUID, and MongoDB still omits it there. That exclusion is pinned by a
test, because it is exactly the sort of asymmetry a later refactor would tidy
away.

`collMod` events also gained `stateBeforeChange` — the collection's options as
they stood before the modification, which is what lets a consumer see what a
`collMod` actually replaced rather than only what it set.

With these, the change-stream differential sweep is at **zero divergences from
MongoDB 8.2.11 across all 41 cases, on both servers**. The one remaining
difference is a field-order quirk in MongoDB's own `rename` event, recorded and
deliberately not copied.
