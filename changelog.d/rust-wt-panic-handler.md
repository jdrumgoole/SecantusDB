### Rust storage: a WiredTiger panic ends the process

#### Fixed

- `secantus-wt` now opens every connection with an event handler. On
  `WT_PANIC`, it prints WiredTiger's message and aborts the process, as
  mongod's handler does (`fassert`). Before, a null handler was passed.
  After a panic every storage call failed, each as an ordinary error, while
  `hello` and `ping` still answered, so a supervisor watching for the
  process to die never saw it. Other errors are still printed by
  WiredTiger's default handler.
- The handler belongs to its connection and lives exactly as long as it.
  WiredTiger writes its defaults into the callbacks left null, so a shared
  read-only handler would crash the process.

Fixes RM-188. `a_wiredtiger_panic_aborts_the_process` runs the handler in a
child process and checks it aborts with the message. The change is in the
shared `secantus-wt` crate, so the Rust PostgreSQL server and the embedded
handles abort on a panic too.
