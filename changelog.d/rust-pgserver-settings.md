### Session settings on the Rust PostgreSQL server

`SET`, `SHOW`, `RESET` and the `current_setting()` / `set_config()` functions
now work, with the settings held per connection as PostgreSQL holds them. This
was the largest remaining thing psycopg's test suite asked for, and the score
moved from 853 to 899 of 4,238.

Small details decide whether a client is satisfied here. `SHOW datestyle`
answers a column named `DateStyle`, not `datestyle` — lookups ignore case but
the reported name does not, and clients match on it. Asking for a setting that
does not exist is an error, while asking with the "missing is fine" flag returns
null instead. `RESET` restores a setting to its default rather than deleting it,
so a client reading it back afterwards sees the default rather than an error.
A fresh connection starts from the defaults, not from whatever the last one did.

#### Added

- `SET` / `SET LOCAL`, `SHOW`, `RESET`, `RESET ALL`.
- `current_setting(name [, missing_ok])` and `set_config(name, value, is_local)`.
- The settings a client expects to read before it has set anything:
  `client_encoding`, `DateStyle`, `TimeZone`, `IntervalStyle`,
  `standard_conforming_strings`, `integer_datetimes`, `transaction_read_only`,
  `search_path`, `application_name`, `server_encoding`, `server_version`.
