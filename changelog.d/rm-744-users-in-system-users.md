### Users and roles live in `admin.system.users` and `admin.system.roles`

`secantusd-mdb` now keeps users and custom roles as documents of
`admin.system.users` and `admin.system.roles`, with mongod's unique
`user_1_db_1` / `role_1_db_1` indexes, instead of tables of its own. mongod
8.2 opening a store this server wrote finds and authenticates its users.
The store format moves to 12; a format-11 store is refused by name.

#### Fixed

- A user or role created on `secantusd-mdb` was invisible to mongod on the
  same store. Fixes RM-744 (tests/backlog/test_rm_storage_model.py::test_mongod_finds_the_users_and_roles_secantus_made)
