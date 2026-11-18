### `$derivative`'s window holds what mongod's does

`explain`'s `maxTotalMemoryUsageBytes` for a `$setWindowFields` `$derivative`
now counts the partition cache as mongod 8.2.11 does.

#### Fixed

- A `$derivative` keeps the documents from its previous answer's lower
  endpoint (mongod's `kEndpoints` accessor): a window with no lower end holds
  the whole partition, `[-3, 3]` eight documents, `[0, 0]` three. The server
  kept only two to four.

Fixes RM-618 (tests/backlog/test_rm_found_2026_11_13_rm509.py::test_a_derivatives_cache_holds_what_mongods_does)
