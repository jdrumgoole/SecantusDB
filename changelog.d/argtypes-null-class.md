### A null command argument no longer crashes `createIndexes`

`createIndexes` with `indexes: null` returned an internal server error instead of
rejecting the argument. Several neighbouring slots were wrong in quieter ways:
`distinct` reported a null key as the wrong type where MongoDB reports it as a
missing required field, and on the Rust server a null was accepted in places
MongoDB rejects it and rejected in two places MongoDB allows it.

These were found by widening the differential test corpus. It had been feeding
three values per argument slot, none of them null — so a sweep reporting every
case clean was accurate and, at the same time, silent about an entire class of
input. The corpus now also covers fractional and negative numbers, decimals,
ObjectIds and dates, and both servers match MongoDB across all 244 resulting
shapes.

Worth knowing if you rely on this behaviour: null is not uniform. MongoDB
accepts it for `find`'s `let` and `listIndexes`' `cursor`, rejects it for
`find`'s `min` and `max` and `aggregate`'s `cursor`, and gives `createIndexes` a
different error code for a null list than for a wrong-typed one.
