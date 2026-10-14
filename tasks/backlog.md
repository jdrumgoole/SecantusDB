# Backlog: the index

The backlog is four files, one per server. This file was the single backlog
until 2026-10-14, when it was split and pruned of finished work; it is kept
as an index because code comments, docs and workflows cite it.

| file | server |
| --- | --- |
| [`backlog-rust-mongodb.md`](backlog-rust-mongodb.md) | the Rust MongoDB server, and the Rust storage both Rust servers use; also CI, build and release items no one server owns |
| [`backlog-rust-postgres.md`](backlog-rust-postgres.md) | the Rust PostgreSQL server |
| [`backlog-python-mongodb.md`](backlog-python-mongodb.md) | the Python MongoDB server, and the admin UI |
| [`backlog-python-postgres.md`](backlog-python-postgres.md) | the Python PostgreSQL / SQL server |

**A citation of this file by section** (`tasks/backlog.md` 7.077, §3.1,
§5) means the section of that number in whichever of the four files the
citing code belongs to; the numbers were kept. A section that held only
finished work was removed: its text is in git history, in the last version
of this file before the split (`git log --diff-filter=M -- tasks/backlog.md`).

Every item in the four files is open work. When you fix one, delete it; when
you stub something, defer a slice or find a limitation, add it to the file of
the server it concerns (each file's header has the conventions).
