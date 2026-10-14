### Rust MongoDB server: user and role commands answer as mongod does

The sixteen user and role management commands on the Rust MongoDB server,
and `usersInfo` and `rolesInfo`, were compared with mongod 8.2.11 reply by
reply. Of 399 replies 277 differed; 7 do now, each of them the order of a
set of roles.

#### Fixed

- `dropRole` removes the role from every user and role that held it. It
  was left named there.
- A built-in role exists only in the databases mongod has it in: `root` on
  a database other than `admin` is `RoleNotFound`, where it was accepted.
- A role name given as `{role: "read"}` with no `db` is refused. It was
  taken to mean the current database.
- `createUser` refuses a mechanism it does not know, where it dropped the
  name, and refuses an empty password, a user on `local`, and a password
  for a user on `$external`.
- A role on a database other than `admin` cannot be granted privileges or
  roles of another database, and a grant that would make a role inherit
  from itself is refused.
- Every field of these commands is checked for its type, and an unknown
  field is refused, with mongod's codes and messages.

#### Added

- `rolesInfo` answers for the built-in roles, with the privileges mongod
  gives each, and takes `showBuiltinRoles`, `showPrivileges` (also
  `"asUserFragment"`) and `showAuthenticationRestrictions`.
- `usersInfo` takes `filter`, `showPrivileges`, `showCustomData` and
  `showAuthenticationRestrictions`, and lists `userId` and `customData`.
- `createUser` and `updateUser` store `customData` and
  `authenticationRestrictions`; `updateUser` can change `mechanisms`; a
  password the client digested is taken for SCRAM-SHA-1.
- `createRole` and `updateRole` store `authenticationRestrictions`, and
  check privileges against mongod's 170 action names.

#### Changed

- A plain `rolesInfo` no longer lists `privileges`; ask with
  `showPrivileges: true`, as on mongod.
- A role that mongod has and enforcement here does not know
  (`hostManager`, `enableSharding`) can now be granted. It permits nothing
  yet (`tasks/backlog.md` 7.083).
