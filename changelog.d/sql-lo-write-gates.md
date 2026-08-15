### Large-object writes now honour RBAC and read-only transactions

PostgreSQL's large-object API reaches the server two ways that both skip
the ordinary statement pipeline — the Fastpath sub-protocol (pgjdbc's
`LargeObjectManager`) and the SQL-callable `lo_*` scalars — so neither the
RBAC gate nor the read-only-transaction check applied to them. A session
with no write privilege, or one inside `BEGIN READ ONLY`, could still
create, write, truncate, or unlink large objects.

Mutating Fastpath calls (`lo_creat`/`lo_create`/`lowrite`/`lo_truncate`/
`lo_unlink`) now pass the same write-privilege check and read-only gate a
table write goes through (large objects are database-scoped, so RBAC is at
db granularity — a write action such as `insert`, which `readWrite`
grants). The `SELECT lo_unlink(...)` scalar path is likewise classified as
a write, so it needs a write grant and is refused inside a read-only
transaction. Read calls and ordinary queries are unaffected.

#### Security

- The Fastpath large-object sub-protocol dispatched `lo_*` writes with no
  authorization or read-only-transaction check (#836). Mutating calls are
  now gated in `_handle_fastpath`.
- The SQL-callable `lo_creat`/`lo_create`/`lo_unlink` scalars slipped the
  read-only gate (a bare `SELECT` reads as non-write) and the write-RBAC
  check. They are now classified as writes on both paths.
