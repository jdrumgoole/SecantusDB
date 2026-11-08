### A malformed command takes no lock, as on mongod

mongod parses a command before it runs it, and takes its locks inside the run, so a command it refuses as malformed never touches the lock manager. The Rust MongoDB server took a command's locks first and found the error afterwards, so `serverStatus.locks` counted locks for requests that never ran. `explain`, `dataSize`, the plan cache commands and `validateDBMetadata` now parse before they lock, through the same parsing code their handlers use. `validateDBMetadata` also takes mongod's locks: one round on each database it reads, and one inside it for each collection, or for the one named. It parses `apiParameters` as mongod does, so a missing `version`, a wrongly typed `strict` or an unknown field is refused instead of answered.

#### Fixed

- A malformed `explain`, `dataSize`, `planCacheClear`, `planCacheClearFilters`, `planCacheListFilters`, `planCacheSetFilter` or `validateDBMetadata` takes no lock, where it took the command's chain. These are mongod 8.2.11's parse errors: the explain's own fields, the explained command and its namespace, `dataSize`'s namespace, bounds and field types, the plan cache commands' namespace and `validateDBMetadata`'s fields.
- `validateDBMetadata` takes mongod's lock rounds. Each database it reads gets a round. Inside an existing one, each collection gets a round, or the one named does whether or not it exists. Views are left out and `system.views` is counted. Before this it took the generic chain.
- `validateDBMetadata` refuses an `apiParameters` without `version` (40414), with a wrongly typed `version` (14), with a non-boolean `strict` or `deprecationErrors` (14, null included), or with an unknown field (40415). It used to answer `ok`. A null `db` or `collection` now counts as absent, where it was refused with 14.
- The plan cache commands refuse an empty collection name with 73 "Invalid namespace specified '<db>'", where `planCacheClear` answered `ok`.
- Fixes RM-185 in part (tests/backlog/test_rm_185_locks.py::test_lock_counts_before_a_command_parses). Replies pinned by tests/mongod_live/test_parse_before_locks.py.

#### Added

- Backlog items RM-431 (an `aggregate` without `pipeline` is not refused with 40414, and with a `cursor` it runs) and RM-432 (`explain` of a write reports the namespace or "Cannot explain" before a missing required field), with gap tests in `tests/backlog/test_rm_431_432.py`. RM-185's remainder (commands whose handler finds a malformed field after taking its locks) is pinned by `tests/backlog/test_rm_185_locks.py::test_lock_counts_of_other_malformed_commands`.
