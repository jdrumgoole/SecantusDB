### Opening a database no longer builds two tables nothing uses

Every time a SecantusDB store opened, it created two WiredTiger tables that no
current code ever writes to: the single documents table that predates
per-collection sharding, and the old forward index that insertion order replaced.
They existed only so a one-time migration and the collection-drop paths could
look at them and find nothing. Each table costs about ten milliseconds to create,
on every open, forever.

They are no longer created. A store that already has them still migrates and
drops from them correctly — every reader treats a missing table as an empty one —
so existing databases are unaffected and remain readable by both servers. Opening
a store is about 9% faster, which shows up wherever a server is started: tests,
short-lived tools, and anything that opens a database per operation.

#### Changed
- The legacy `secantus_documents` and `secantus_natural` tables are no longer
  created when a store is opened, on either server.

#### Fixed
- A pre-existing clippy warning in `secantus-storage`, which sits outside the
  clean-workspace lint gate and so had gone unreported.
