Title: secantus-mdb 0.5.3-beta.193 checks a collection in validate, and answers user and role commands as mongod does
Date: 2026-10-14 06:00:00
Slug: secantus-mdb-0-5-3-beta-193
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-mdb 0.5.3-beta.193 makes validate read the collection and its indexes, removes a dropped role from the users that held it, and answers the user and role management commands as mongod does.

Upgrade if you run `secantusd-rs` and your tests create users or roles, or
call `validate`.

`validate` checks the collection now. The last release's post said it
answered `valid: true` without reading anything. It now reads every
document and compares each index with the entries its documents should
have, in both directions. It answers `valid: false`, with the counts in
`errors`, when a document cannot be read, an index entry is missing, or an
entry belongs to no document. `keysPerIndex` is the number of keys the
documents give each index. It was the number of documents.

Five answers about users and roles were wrong:

- **`dropRole` left the role named** on every user and role that held it.
  It is removed from them now.
- **A built-in role existed on every database.** `createUser` with
  `roles: ["root"]` on a database other than `admin` was accepted. It is
  `RoleNotFound` now, because `mongod` has `root` on `admin` only.
- **`{role: "read"}` with no `db` meant the current database.** `mongod`
  refuses a role document without a `db`, and so does this server now.
- **`createUser` dropped a mechanism name it did not know** and created
  the user. It refuses the name now. It also refuses an empty password, a
  user on `local`, and a password for a user on `$external`.
- **A role could be granted the privileges and roles of another
  database**, and could be made to inherit from itself. Both are refused,
  except for a role on `admin`, which may hold either.

`rolesInfo` knows the built-in roles now, with the privileges `mongod`
gives each. It takes `showBuiltinRoles`, `showPrivileges` and
`showAuthenticationRestrictions`. `usersInfo` applies its `filter`, which
it ignored, and lists `userId` and `customData`. `createUser`,
`updateUser`, `createRole` and `updateRole` store `customData` and
`authenticationRestrictions`. `updateUser` can change a user's
`mechanisms`.

One reply changed shape. A plain `rolesInfo` no longer lists
`privileges`. Ask with `showPrivileges: true`, as on `mongod`. A test that
read `privileges` from a plain `rolesInfo` needs that option.

Every field of these commands is checked for its type, and an unknown
field is refused, with `mongod`'s codes and messages.

We found these by sending 399 user and role commands to this server and
to `mongod` 8.2.11 and comparing each reply. The previous release differed
on 277. This one differs on 7, each of them the order a set of roles is
listed in.

What this release does not do. It changes what the commands answer and
store, and it does not change what an authenticated user is allowed to
run. Two limits there are worth knowing if you start the server with
`--auth`:

- **A privilege on one collection covers the whole database.** A role
  with `insert` on `app.orders` lets its user insert into any collection
  of `app`. On `mongod` it covers `orders` only.
- **Some built-in roles permit nothing.** `hostManager` and
  `enableSharding` can be granted now, and a user who holds only one of
  them can run no command it should allow.

Users and roles written by the previous release still work. We created
three users and a role with the 0.5.3-beta.192 binary, opened the store
with this one under `--auth`, and each user logged in.

We ran the pymongo, Go and Node driver suites against this code. Each gave
the same result as the release before.

This is a server for tests. It is single-node and it is a beta.

`cargo install secantus-mdb --version 0.5.3-beta.193` builds it from crates.io.
Binaries for Linux x86_64, macOS arm64 and Windows x86_64 are on the release.

[Rust MongoDB server](https://secantusdb.com/rust-db.html) ·
[secantus-mdb on crates.io](https://crates.io/crates/secantus-mdb) ·
[MongoDB binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusdb-v0.5.3-beta.193)
