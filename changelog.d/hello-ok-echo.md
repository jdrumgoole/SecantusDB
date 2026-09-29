### Drivers now speak modern `hello` instead of falling back to legacy `isMaster`

A driver puts `helloOk: true` in its opening handshake to ask whether the server
understands the modern `hello` command; a server that does echoes `helloOk: true`
back, and the driver uses `hello` from then on. Neither SecantusDB server echoed
it, so every modern driver concluded the server predated `hello` and fell back to
the legacy `isMaster` — on every connection it opened, for the life of that
connection.

That was visible on the wire rather than inferred: captured through a logging
proxy, mongo-go-driver's SDAM monitor sent `isMaster exhaustAllowed` to
SecantusDB and `hello exhaustAllowed` to a real mongod, and did no RTT
monitoring against us at all. Both servers now echo the flag, and only when the
client asks for it — mongod omits it otherwise, so echoing unconditionally would
be its own divergence.

#### Fixed
- `hello` / `isMaster` echo `helloOk: true` when the client's request carries it, over both the initial OP_QUERY handshake and OP_MSG, matching mongod 8.2.11.
