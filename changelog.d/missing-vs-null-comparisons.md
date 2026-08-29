### A missing field compared equal to null in every aggregation comparison

A targeted sweep, motivated by the Phase 2 differential campaign: `get_path`
returns `None` for both an absent field and an explicit null, so anywhere the
code tested a resolved value for `is None` it had conflated them. That
confusion produced both `$graphLookup` bugs and the `$lookup` `let` gap found
earlier in the campaign, so the question was how far it spread.

25 shapes probed against mongod 6.0.16. **22 were already right** — the *query*
language deliberately does treat them alike (`{a: null}` matches a missing
field, and that is mongod's behaviour too). The divergence was confined to the
**comparison operators**.

#### Fixed

- **`$eq: ["$absent", null]` answered true**; mongod answers false, while
  `$eq: ["$explicitNull", null]` is true. Every comparison against null matched
  documents that did not have the field at all, and `$cond` built on `$eq`
  inherited it.
- **A missing field now ranks below every real value**, MinKey included
  (`$cmp: ["$absent", MinKey]` is `-1`), and equals only another missing field.
  `$ne` / `$lt` / `$lte` / `$gt` / `$gte` / `$cmp` all follow.
- **`$let` bound a missing field as null.** A var bound from an absent field
  now stays missing, so `$eq: ["$$v", null]` is false the way mongod's is.
- **`$lookup`'s `let` had the same bug**, which is how it was found: a document
  without the local field joined foreign rows mongod excludes. This closes the
  gap recorded when `$lookup` was swept.

The Rust engine carries the identical rule — the parity fuzz caught the
divergence within seconds of the Python change, which is exactly its job.

#### Corrected

`_eval_field_value`'s docstring (and its Rust mirror) claimed
`{$add: ["$nope", 1]}` is `1`. mongod answers `null`; arithmetic over null is
null. The *behaviour* was already right — only the note about it was wrong, and
a test written from that comment failed against the real server. That is the
sixth instance in this campaign of a comment asserting something the oracle
contradicts.
