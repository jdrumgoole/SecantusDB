### The last two Rust-server backlog items, measured and decided

Neither is a coding task. Both entries asserted a blocker; probing replaced the
assertions with data, and in both cases the data changed the conclusion.

**`Decimal128` transcendentals — a decision, not a dependency gap.** The entry
said this "needs a 34-digit decimal library" and suggested tabling the zero
column as a cheap partial win. Measured against mongod 8.2.11:

- The **zero column is already done** — 0 divergent of 18, as are all six error
  cases. There was nothing to win.
- The library is **not the blocker**. The Python server already has one (stdlib
  `decimal` at full precision) and still differs from mongod by exactly one unit
  in the last place on `$ln`, `$log10` and `$sin`, while matching on six others.
  mongod uses Intel's RDFP, whose transcendental rounding differs from every
  other implementation in the final digit.

So the real choice is: link Intel RDFP (exact, but a C dependency across five
wheel platforms), add a pure-Rust decimal crate (cheap, but silently wrong in
the last digit), or keep refusing. The middle option is the one to resist without
an explicit decision — it trades a visible refusal for an invisible wrong answer.

**`$toDate` of an unparseable string — won't fix.** mongod's message is
`timelib`'s re2c scanner talking. Across 25 measured strings there is no small
rule: `'a1'` errors at position 1 while `'1a'` errors at position 0, `'123'`
emits one error *per position*, `'junk here'` emits two with a "Double timezone
specification", and `'2020-01-01X'` **parses** because `X` is the military
timezone UTC+11. Reproducing it means reproducing the lexer, its timezone
abbreviation tables and its per-position error accumulation; a partial job would
emit messages mongod never sends.

Both entries are rewritten with the measurements so the next session inherits
evidence instead of an assertion.
