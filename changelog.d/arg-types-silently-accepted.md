### Wrong-typed command arguments are no longer accepted in silence

Nine argument slots took a value of the wrong type without complaint and
reported success. `find` accepted `collation: 5` and `let: "x"`, `create`
accepted `storageEngine: true`, `collMod` accepted `index: 5`, `aggregate`
accepted `let: 5`, and the boolean slots — `find`'s `singleBatch`,
`findAndModify`'s `upsert`, `update`'s `multi` — took objects and arrays. A
driver sending a malformed command was told the operation had succeeded, which
is the worst of the three ways this class of bug shows up: the earlier tranches
either crashed the command or answered the wrong error code, and both of those
are at least visible.

All nine now answer what MongoDB answers, and `find`'s `maxTimeMS` — the one
slot in the sweep that is not a type error at all — answers its three distinct
`BadValue` messages for a non-number, a non-integral number, and a negative one.
The `let` option on `update`, `delete` and `findAndModify` is validated too.

Two related divergences turned up while probing and are fixed with them: an
explicit `null` was accepted for `find`'s `filter`, `sort` and `projection`, and
for `aggregate`'s `cursor`, where MongoDB rejects it. An *absent* option is
still fine — the two cases had been indistinguishable in the code.

Nine slots needed six different message families, because MongoDB's strictness
here is per-slot rather than per-class. `findAndModify.upsert` accepts a bool or
any number, so `upsert: 1` and even `upsert: 1.5` are valid, while the adjacent
`update.multi` is a strict bool that rejects `multi: 1`. `find.let` is reported
under MongoDB's internal name `FindCommandRequest.let` while the same option on
every other command uses the command's own name. Six slots accept an explicit
`null` and three reject it. Every one of these was measured against a live
`mongod` rather than inferred from its neighbour, and `delete`'s `limit` — which
MongoDB genuinely does not type-check — is pinned by a test so a future tidy-up
can't sweep it in.
