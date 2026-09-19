### Ruling out the explanation that would have been ours

Two psycopg tests once failed in forty milliseconds where they should have taken
four seconds, and an earlier note blamed the machine's network for answering an
unroutable address immediately. That note was wrong and has been withdrawn. The
question it left behind was better: an immediate `connect()` failure is exactly
what a harness that has run out of sockets produces, and that would be a defect
here rather than a property of the machine.

Measured during a full gauge run, it is not what happens. Ephemeral port use
peaks at eleven hundred of the sixteen thousand available and sawtooths rather
than climbing, sockets in `TIME_WAIT` rise and fall with the work, and the
descriptor limit on this platform is a million. Nothing is running out.

The failure is therefore still unexplained, with its most plausible cause
eliminated by measurement instead of argument, and the backlog now says both
halves of that. The probe is kept so the next person measures rather than
reasons.

#### Added

- `tools/probes/socket_pressure.py`: samples descriptors, `TIME_WAIT` sockets
  and ephemeral-port use while a long run proceeds, and reports whether any of
  them climbs.
