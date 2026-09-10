### `secantusd-pg` reports the address it actually bound

The readiness line exists so a harness can wait for the server and then connect,
but it echoed the address that was *requested* rather than the one the listener
ended up on. That made `127.0.0.1:0` useless: the kernel picks the port and
nothing told the caller which one, so callers had to probe for a free port
themselves and pass it in — a race no caller can win, because the probe socket
must be closed before the child can bind it.

#### Fixed

- `secantusd-pg`'s `listening on …` line now reports `local_addr()`, so
  `127.0.0.1:0` is a supported way to start the server and read back its port.
- The test harness now starts every `secantusd-pg` that way instead of guessing
  a port. Under `pytest -n auto` two workers could be handed the same port; the
  loser's child exited, but a liveness probe fired in that window connected to
  the *winner's* server, and the test then ran against another worker's database
  until that worker shut it down — surfacing as
  `server closed the connection unexpectedly` on an opening `CREATE TABLE`.
