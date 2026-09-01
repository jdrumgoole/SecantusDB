### A `let` variable made a pipeline update fail

Passing `let` to `update` or `findAndModify` and referring to it from a pipeline
update — `{update: "c", let: {x: 5}, updates: [{q: {}, u: [{$set: {v: "$$x"}}]}]}`
— was refused with `Use of undefined variable: x`, for a variable the command
itself defines. `update` reported it as a per-statement write error and applied
nothing; `findAndModify` failed the whole command.

The variable *was* bound. What went wrong is in the parse-time check that
reproduces MongoDB's constant folding: it decides an expression is constant
because every `$$name` in it is bound, then evaluates it — and the update path
passed the bound *names* without the *values*, so the evaluation hit an unbound
variable and reported that as the query's error. `aggregate` passed both and was
unaffected, which is why this only ever showed up on writes. The fold now
declines to fold a variable it has no value for, and both write paths pass the
values the way `aggregate` does.

Two more differences surfaced alongside it. A statement's own `c` constants map
was never bound at all, so `{u: [...], c: {y: 7}}` failed the same way — and
MongoDB rejects `c` outright on a non-pipeline update, which we accepted. And a
constant that genuinely does fail (`{$abs: "$$cv"}` with `cv: "x"`) takes
MongoDB's *executor* prefix inside a pipeline update, naming the command, rather
than `aggregate`'s optimizer prefix.

Finally, neither write command validated its statements' field names. MongoDB
answers `40415` for anything it does not know inside a `delete` or `update`
statement — including `$`-prefixed names, which get no envelope carve-out there
— and refuses the whole command rather than the one statement, because it parses
every statement before running any.

Found by the pymongo gauge: three `*_with_let_option` tests in
`test_crud_unified.py`.

#### Fixed

- `aggregate.py`: the constant-fold check no longer reports a bound-but-unvalued
  variable as undefined, and `wrap_expression_problem` takes the command name
  for the update-pipeline executor prefix.
- `commands.py`: `update` and `findAndModify` pass the real `let` values to the
  fold; a statement's `c` constants are bound (and win over `let`); `c` on a
  non-pipeline update answers `51198`; unknown fields in a `delete` / `update`
  statement answer `40415`.
