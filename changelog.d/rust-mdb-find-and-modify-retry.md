### Rust MongoDB server: a retried findAndModify answers from what the first wrote

#### Fixed

- **A retried `findAndModify` is decided by mongod's rules** (RM-203,
  `validateFindAndModifyRetryability`, measured on mongod 8.2.11).
  - **Earlier write an insert** (an upsert, or a plain `insert`): only an
    upsert retry is answered, with the inserted document when `new` asks.
  - **Earlier write an update:** the retry gets the document kept the first
    time, before or after the update, whatever this request's `new` says.
    A remove retry gets it too.
  - **Earlier write an update statement:** no document was kept, so the
    retry is refused (40611 / 40612).
  - **Earlier write a delete:** only a remove is answered.

  Before, an upsert retried after an insert was refused, and a retry
  asking the other `new` was answered by the stored reply only by chance.
- **A retried write after a driver's insert names the inserted `_id`.**
  The `_id`s of an insert sent as an `OP_MSG` document sequence, as every
  driver sends one, were not recorded. A retried upsert answered
  `upserted: null`.
- Tests: `tests/backlog/test_rm_057_084.py::test_a_retried_find_and_modify_answers_from_what_the_first_wrote`
  covers 8 cases, and a 16-case raw-wire comparison matches apart from
  message text, which remains RM-087.
