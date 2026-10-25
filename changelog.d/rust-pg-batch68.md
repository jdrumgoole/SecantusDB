### Rust PostgreSQL server: a write's heap buffers, and SELECT INTO

`pg_statio_user_tables` counts the heap buffers each kind of write reads,
as PostgreSQL 15.19 counts them. `SELECT ... INTO` creates its table.

#### Fixed

- A write counts the heap buffers PostgreSQL's would, where every row
  written counted one (RP-239). The heap model notes each page it reads
  as it places a row:
  - **INSERT:** the target page, and when that page is full, the free
    space map's leaf and root (`RecordAndGetPageWithFreeSpace`), then the
    new page. So 2,000 rows into an empty table read 2,102 buffers.
  - **UPDATE:** the old version fetched to form the new one, the page
    `heap_update` reads, and a new page when the row moves. The old page,
    already held, is not read again.
  - **DELETE:** each row's page (`heap_delete`).
  - **COPY:** writes through a bulk-insert state that holds its page and
    keeps the free space map, or skips it for a table new in the
    transaction.
  - **CREATE TABLE AS and SELECT INTO:** write through a bulk-insert state
    that skips the free space map (`TABLE_INSERT_SKIP_FSM`), one buffer per
    page. They no longer record free space as they fill, as PostgreSQL's
    do not.
  - **The buffer cache:** a free space map page counts against the table,
    apart from the heap page of the same number.

  Fixes RP-239 (tests/backlog/test_rp_batch63.py::test_scenario[a write counts the heap pages PostgreSQL's buffer accesses do],
  tests/backlog/test_rp_batch68.py::test_scenario[INSERT reads its target page and the free space map],
  tests/backlog/test_rp_batch68.py::test_scenario[UPDATE reads the old version twice],
  tests/backlog/test_rp_batch68.py::test_scenario[UPDATE of every row reads the old versions and new pages],
  tests/backlog/test_rp_batch68.py::test_scenario[DELETE reads each row's page],
  tests/backlog/test_rp_batch68.py::test_scenario[COPY writes through a bulk insert],
  tests/backlog/test_rp_batch68.py::test_scenario[CREATE TABLE AS and SELECT INTO skip the free space map]).
- `SELECT ... INTO [TEMP | UNLOGGED] t` creates `t` from the query, as
  CREATE TABLE AS does, and tags `SELECT n` (RP-246). Before, it answered
  the query's rows and made no table. An UNLOGGED CREATE TABLE AS makes an
  unlogged table, where it made a logged one.

  Fixes RP-246 (tests/backlog/test_rp_batch68.py::test_scenario[SELECT INTO creates its table]).

#### Known gaps

- An UPDATE of every row applies them in storage order, not heap order, so
  after a row has moved their new places differ from PostgreSQL's (RP-247,
  open).
