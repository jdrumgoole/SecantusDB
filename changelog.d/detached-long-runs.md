### Long runs survive the shell that launched them

A multi-minute test or gauge run started in a background shell died three
times in one session — twice at about ninety per cent, once at a third of the
way through — with no failing test, no out-of-memory kill and nothing in the
log but the word `killed`. The cause was not the run: a supervisor reaping the
shell it had spawned signalled the entire process group, and the work shared
that group.

`scripts/detached_run.py` gives the work its own session and process group, so
reaping the launcher leaves it running. Start a command under a name, then poll
it, wait on it, or stop it; the pid, the command line, the log and the eventual
exit code live in `.detached-runs/<name>.json` beside `<name>.log`, which is
what makes the exit code readable at all once the launching process is gone.

#### Added

- `scripts/detached_run.py` with `start` / `status` / `wait` / `stop`
  subcommands for long-running commands that must outlive their launcher.
- `tests/test_detached_run.py`, which pins the property that matters by
  signalling the launcher's own process group and asserting the child is
  untouched, plus exit-code capture and the refusal to reuse a live name.
