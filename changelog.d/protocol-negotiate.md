### Newer wire-protocol minors negotiate down instead of dropping the connection

A client that opened with any protocol version other than exactly 3.0
had its connection dropped at the startup packet. pgx 5.6+ offers
protocol 3.2 by default configuration (`MaxProtocolVersion`), and real
PostgreSQL answers a newer minor with `NegotiateProtocolVersion` — the
newest minor it speaks plus any unrecognized `_pq_.*` startup options —
then both sides continue at the negotiated version.

The server now accepts any major-3 startup, sends
`NegotiateProtocolVersion` as the first response when the client asked
for a newer minor (or sent unknown `_pq_.*` options), and continues the
handshake at 3.0. `SHOW server_version_num` is also supported (150000,
matching the advertised 15.0), which protocol-aware clients read to
pick expected wire shapes.

#### Fixed

- `sql/pgwire.py`: startup packets accept any major-3 protocol and carry
  the requested version; `negotiate_protocol_version` builds the 'v'
  message.
- `sql/pgserver.py`: the handshake answers a newer minor / unknown
  `_pq_.*` options with NegotiateProtocolVersion first, like real PG.
- `sql/session.py`: `server_version_num` GUC (150000).
