### A killed mongod store is judged as its replay leaves it

`secantusd-mdb` refused a store mongod was killed on when its stable
checkpoint listed something this server cannot serve (a `text` index, say)
that the oplog after the checkpoint drops. mongod's restart replays the drop
before it serves anything, and so does this server's judgement now.

#### Fixed

- An index or collection an oplog entry after the recovery point drops (and
  no later entry makes again) is neither refused nor adopted at the open of
  a store mongod wrote; the replay's drop then finds nothing to do.

Fixes RM-756 (tests/backlog/test_rm_storage_model.py::test_secantus_opens_a_killed_mongod_store_whose_oplog_drops_what_it_refuses)
