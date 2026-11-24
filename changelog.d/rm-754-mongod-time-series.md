### A store mongod wrote with a time-series collection opens

`secantusd-mdb` opens a store mongod 8.2 wrote that holds a time-series
collection, and serves its measurements and its indexes as mongod does,
instead of refusing the store.

#### Fixed

- The registry rows of a time-series collection are rebuilt from mongod's
  catalog as the store opens: the collection's options from those mongod
  keeps for `system.buckets.<name>`, the buckets' own, and each index of
  the buckets listed on the collection on its own fields. An index of the
  buckets that no index of the collection gives is refused by name.

Fixes RM-754 (tests/backlog/test_rm_storage_model.py::test_secantus_opens_a_mongod_time_series_collection)
