### The Rust server renders `$toLower` / `$toUpper` of a `Timestamp`

mongod puts a `Timestamp` through a legacy `asctime`-like path rather than the
`$dateToString` format language, and renders it in the **server process's local
timezone**. The Python server has always matched that; the Rust server answered
`16007 can't convert from BSON type timestamp to String` instead — a refusal
where mongod returns a string, and one with no Python behind it on the
standalone server.

#### Fixed

- `$toLower` / `$toUpper` of a `Timestamp` now render on the Rust server, as
  `%b %e %H:%M:%S:<increment>` — the day space-padded (`jul  3`), the increment
  unpadded (`:0`, `:12`), the whole string then ASCII-cased.
- The rendering is DST-correct, resolved against a real timezone database at the
  instant rather than a fixed offset: re-probed against mongod 8.2.11 across
  three zones, `America/New_York` is 5h behind in November and 4h in July.
  Verified end-to-end over the wire on `secantusd-rs` under both `TZ=UTC` and
  `TZ=America/New_York`.
