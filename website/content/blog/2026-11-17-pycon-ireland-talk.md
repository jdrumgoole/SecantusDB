Title: Building SecantusDB: the PyCon Ireland talk
Date: 2026-11-17 18:00:00
Slug: pycon-ireland-talk
Author: Joe Drumgoole
Category: Talks
Tags: talk, pycon

Summary: The slides for "Building SecantusDB" at PyCon Ireland 2026: two Rust database servers that speak MongoDB and PostgreSQL over WiredTiger, and the Python prototype that got us there.

The slides for my PyCon Ireland 2026 talk, **Building SecantusDB**, are
online: **[open the deck](/talks/pycon-ie-2026/index.html)**. Use the arrow keys
or the buttons in the corner to move between slides.

The talk runs for 30 minutes and tells the story of building SecantusDB:

- **What it is.** A real single-node database: two Rust servers,
  `secantusd-mdb` and `secantusd-pg`, that speak the MongoDB and
  PostgreSQL wire protocols over WiredTiger, the storage engine MongoDB
  ships. Run them standalone, like `mongod` or `postgres`, or start one
  from Python in a line.
- **The Python prototype.** How the first version, written in Python,
  learned the protocol and settled the design: documents stored as opaque
  BSON blobs, with every decision measured against what `pymongo` sends.
- **How we know it is right.** Every behaviour is measured against the
  real server, `mongod` 8.x or PostgreSQL, never against our own code, and
  20 unmodified upstream driver test suites run against both servers.
- **The move to Rust, and a second protocol.** Why the Python servers were
  retired, and how the PostgreSQL server reuses the same storage.
- **Process.** Why an error is never noise in a database, a backlog where
  every open item is a failing test, and building with AI agents.

To try it:

```bash
pip install SecantusDB
cargo install secantus-mdb --version 0.5.3-beta.198
cargo install secantus-pg --version 0.1.0-beta.20
```

Prebuilt binaries are on the GitHub releases page.
