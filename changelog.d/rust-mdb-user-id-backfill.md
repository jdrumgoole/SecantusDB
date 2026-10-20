### Rust MongoDB server: stored users get a `userId`

#### Fixed

- **Old user records get a `userId`** (fixes RM-031). A user stored before
  records carried a `userId` gets one when the server starts, after `_id`
  where mongod keeps it, and `usersInfo` lists it from then on. Test:
  `crates/secantus-storage-adapter/tests/user_role_commands_wt.rs::a_stored_user_without_a_user_id_gets_one`.

#### Changed

- RM-030, the order of a set of roles, is now a scope decision. mongod keeps
  role sets in a hash set and gave three orders for one grant in one
  process, so no fixed order is mongod's.
- RM-032, whether the Python server reads the new user record, is retired.
  Python-server compatibility stopped being a constraint on 2026-10-16.
