### A command refused before it runs is not a connection's first command

mongod logs `Received first command on ingress connection` once per
connection, for the first command after the handshake that gets as far as
running. A command it refuses on the way -- a namespace that does not parse,
an unknown generic argument, a bad read or write concern, a `maxTimeMS` of
the wrong type, a refused authorization, a `failCommand` failpoint -- is
passed over, and the next command writes the line, timed from the `hello`.
The Rust MongoDB server used to count every command it knew as it arrived;
it now writes the line where mongod does, just before the command runs, so a
command its dispatcher refuses no longer takes the line from the one after.

#### Fixed

- `secantus-commands`: the 6788700 line is written after the dispatcher's
  checks, where the command is about to run, instead of on arrival; its
  `elapsedMillis` is still timed from when the command arrived.
  Fixes RM-386 in part (tests/backlog/test_rm_001_028.py::test_a_command_that_fails_to_parse_is_not_the_first_command);
  a command refused by its own handler's parse (an unknown field, a field
  of the wrong type, an unknown pipeline stage, a malformed `lsid`) still
  counts, and stays open under RM-386.
