### Change streams report array edits the way MongoDB does

An update that changed an array produced an `updateDescription` MongoDB would
never send. Popping one element reported a `truncatedArrays` truncation;
pushing one reported the entire array. MongoDB does the opposite of both: it
resends the whole array for a `$pop`, and reports `arr.5` for a `$push`.

The reason it was wrong in both directions is that the shape depends on the
**operation**, not on what changed. `$set: {arr: [1,2,3,4,5,6,7]}` and
`$push: {arr: {$each: [6,7]}}` produce an identical document, and MongoDB
reports the first as a whole-array replacement and the second as two indexed
appends. No comparison of the before and after documents can tell those apart,
so the update itself is now an input to the diff.

Measured against MongoDB 8.2.11 and applied to both servers:

- `$push` / `$addToSet` report `arr.<i>` for each appended index;
- `$set`, `$unset`, `$inc`, `$mul`, `$min` and `$max` of an indexed path report
  exactly that path — `$set: {"arr.7": 77}` on a five-element array reports
  `arr.7` and not the two nulls it silently creates;
- `$pop`, `$pull`, `$pullAll`, a sliced or sorted `$push`, and a whole-field
  `$set` report the whole array;
- an aggregation-pipeline update is diffed by value, and that is the one shape
  where `truncatedArrays` really is emitted.

That last point corrects a claim made when this was filed. The earlier sweep
concluded MongoDB "never" emits `truncatedArrays`, having probed only operator
updates; it emits it for pipeline updates, which is what the driver spec suite
had been asserting.

Ten of the fourteen remaining differences in the change-stream sweep close with
this. The four that remain are a separate, recorded gap in expanded events.
