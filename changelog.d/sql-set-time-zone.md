### `SET TIME ZONE` actually sets the time zone

Written the two-word way — `SET TIME ZONE 'Europe/Dublin'` — the statement did
nothing at all. It takes no `=` or `TO`, so it slipped past the handler that
reads name-and-value settings, and `SHOW TIME ZONE` answered with an empty
string because that spelling was not recognised either. A client that pinned
its connection's zone this way, as JDBC drivers do, silently stayed on the
default and had no way to tell.

Both spellings now set and report the same setting, `DEFAULT` resets it, and
the change is announced to the client the way other tracked settings are.

Worth being clear about the limit: this makes the *setting* stick. Values of
type `timestamp with time zone` are still stored and displayed without regard
to it — that conversion is a larger piece of work and is written up in the
backlog.

#### Fixed

- `SET TIME ZONE <value>` sets the `TimeZone` setting; `SHOW TIME ZONE` reports
  it.
