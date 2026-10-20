### The authorization probe compares against a replica-set mongod

#### Changed

- `tools/probes/auth_enforcement.py` starts mongod as a one-member replica
  set (with a key file, initiated through the localhost exception before
  the first user) and with `--noscripting`, matching what the Rust server
  reports itself to be and the decision to have no JavaScript engine. This
  closes RM-024: 32 replies differed only because the reference was a
  standalone. The Rust server now differs on none of 12,053 checks.
