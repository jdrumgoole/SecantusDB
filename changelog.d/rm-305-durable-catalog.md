### The Rust MongoDB server logs mongod's durable catalog lines at startup

After the spill engine opens, `secantusd-mdb` now writes mongod 8.2.11's
`Initializing durable catalog` (9529901), `Retrieving all idents from
storage engine` (9529902) and `Initializing all collections in durable
catalog` (9529903) on `initandlisten`, the first and last carrying the
number of collections in the catalog, as mongod's do.

#### Fixed

- The startup log lacked mongod's three durable-catalog lines.
  Fixes RM-305 in part (tests/backlog/test_rm_001_028.py::test_startup_initializes_the_durable_catalog_after_the_spill_engine)
