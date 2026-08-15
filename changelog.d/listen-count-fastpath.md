### The notification-push check stays off the hot path

The async LISTEN/NOTIFY push decides per message read whether a session
is a listener. That check took the server-wide notify-hub lock and
scanned the channel registry — on every message, on every connection, a
shared lock on the whole server's hottest path. It now reads a
per-session counter maintained by the hub at LISTEN / UNLISTEN time: a
plain attribute read, no lock, no scan.

#### Changed

- `sql/pgnotify.py` / `sql/session.py`: `is_listening` reads
  `Session.listen_count` (maintained under the hub lock by
  `listen` / `unlisten` / `unlisten_all`) instead of locking and
  scanning the channel registry per message read.
