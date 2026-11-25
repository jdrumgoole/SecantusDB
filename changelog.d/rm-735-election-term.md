### A restart is an election: the term rises

Each start of a one-member set after its initiation is an election in mongod
8.2.11, which raises the term by one. `secantusd-mdb` now does the same: it
records the new term in `local.replset.election` and `local.system.replset`,
logs `{op: "n", o: {msg: "new primary"}}` at it, and every later oplog entry
carries it as its `t`. `hello` (`electionId`, `lastWrite`), `replSetGetStatus`,
write replies' `opTime` and `config.transactions` report it too. A prepared
transaction committed after a restart points its `prevOpTime` at the
prepare's own term.

#### Fixed

- The election term stayed 1 for the life of a store, so a store mongod had
  raised to term 2 got entries at `t: 1` after mongod's `t: 2` ones.
  Fixes RM-735 (tests/backlog/test_rm_storage_model.py::test_a_restart_raised_the_election_term)
