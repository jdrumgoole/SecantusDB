### Rust pgserver: inet and cidr

The Rust PostgreSQL server now supports the network address types `inet` (oid
869) and `cidr` (oid 650) — as casts, bound values, and real column types.
Both are carried as the canonical `addr/masklen` text the Python server stores,
so the two servers share one representation. `inet` keeps its host bits; `cidr`
is strict and rejects any host bit set below the netmask. IPv4 and IPv6 are
both handled, with IPv6 compressed to its canonical form.

psycopg sends and reads these types in the binary wire format, so the round
trip goes through PostgreSQL's `[family][bits][is_cidr][nb][addr]` layout in
both directions: a `/32` (or `/128`) host comes back as an `IPv4Address` /
`IPv6Address`, a shorter prefix as an `Interface`, and a `cidr` as a `Network`,
exactly as against a real server. The `::text` cast keeps the mask
(`network_show`), while an inet column read in text drops a full-host mask
(`inet_out`).

#### Added
- `inet` (869) and `cidr` (650) casts, bound values (text + binary), and column
  types; malformed input is `22P02`, and a `cidr` with host bits set is `22P02
  invalid cidr value`.
