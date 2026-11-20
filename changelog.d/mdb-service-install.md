### `secantusd-mdb service install` runs the MongoDB server in the background

`secantusd-mdb service install [SERVER OPTIONS...]` makes the Rust MongoDB
server a per-user background service, started at login, under the operating
system's own service manager: a launchd LaunchAgent on macOS, a systemd user
unit on Linux. `service status` reports whether it is running and where it
listens; `service uninstall` stops it and keeps the data. Everything lives
under `~/.secantus`. The server also takes mongod's spellings `--dbpath` and
`--bind_ip`.

#### Added

- `secantusd-mdb service install | uninstall | status`. `install` copies the
  binary to `~/.secantus/bin`, writes the service file, starts the service
  and waits until the server says it is listening; the options after
  `install` are the server's own, with the store at `~/.secantus/data` unless
  one is named, and the log at `~/.secantus/secantusd-mdb.log`. Installing
  again replaces the service and keeps the data. `--dry-run` prints the
  service file and the commands without running them. `status` exits 0 when
  the service is running and 3 when it is not (the LSB status code).
- Like mongod's packaged unit and Homebrew's mongod plist, the service does
  not restart a server that exits, raises the open-file limit to 64000
  (launchd's default is 256, too few for a store with a table per
  collection and per index), and allows 90 seconds for a clean stop
  (launchd's default is 20).
- `--dbpath` and `--bind_ip`, mongod's names for `--storage-path` and
  `--host`, reported by `getCmdLineOpts` under the same paths. `--bind_ip`
  takes one address: a list, which mongod binds, is refused by name.
- `mongod --fork` was considered first and set aside: mongod 8.2.11 and
  8.3.4 refuse it on macOS (`BadValue: Server fork+exec via --fork ... is
  incompatible with macOS`), so a service manager is the one approach that
  works on both platforms.
