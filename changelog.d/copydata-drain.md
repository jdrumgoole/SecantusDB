### Stray COPY frames no longer poison the connection

Drivers that stream COPY data concurrently with the command — pgx's
`CopyFrom` pumps `CopyData` without waiting for the server's
`CopyInResponse` — kept sending frames after the COPY command itself had
already failed (a syntax error, a missing table). The wire server routed
those stray frames into the extended-protocol dispatch, answered
`08P01 unexpected message type 'd'`, and left the connection in a
discard-until-Sync state that a simple-protocol client can never clear:
one failed COPY wedged the connection for good.

Real PostgreSQL accepts and silently discards `CopyData`, `CopyDone`,
and `CopyFail` messages that arrive outside a COPY operation, exactly so
that this optimistic-streaming pattern stays safe. The wire server now
does the same, so a failed COPY reports its error and the connection
remains fully usable — including an immediately following valid COPY.

#### Fixed

- `sql/pgserver.py`: `CopyData` / `CopyDone` / `CopyFail` frames arriving
  outside a COPY operation are accepted and discarded, matching
  PostgreSQL's `PostgresMain` behaviour, instead of raising `08P01` and
  poisoning the extended-protocol state.
