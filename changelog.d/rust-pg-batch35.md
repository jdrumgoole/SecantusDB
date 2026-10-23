### Rust PostgreSQL server: the server's own settings, and who may read and set them

Every parameter answers `SHOW` as in PostgreSQL 15.19, and the server's
file locations and port are its own. `GRANT SET` and `GRANT ALTER SYSTEM ON
PARAMETER` work, and they are what let a role other than a superuser set a
superuser-only parameter.

#### Added

- `GRANT / REVOKE { SET | ALTER SYSTEM | ALL } ON PARAMETER`, kept in the
  shared `pg_parameter_acl` with PostgreSQL's `aclitem`s, grantor included.
  Fixes RP-164
  (`tests/backlog/test_rp_batch33.py::test_scenario[GRANT SET and ALTER SYSTEM ON PARAMETER]`
  and `tests/backlog/test_rp_batch35.py::test_scenario[parameter grants, grant options and their revocation]`).
  - **Grant options:** a role holding one grants on in its own name.
    `REVOKE ... CASCADE` takes those grants with it; without CASCADE it is
    2BP01 `dependent privileges exist`. The warnings for what could not be
    granted or revoked are PostgreSQL's.
  - **Checks:** `has_parameter_privilege` answers from the grants, and
    `ALTER SYSTEM` accepts a role granted it on the parameter.
  - **Roles:** DROP ROLE names a role's parameter grants among what depends
    on it, and DROP OWNED revokes them.
- The data directory's `postgresql.conf` is read when the store has one,
  below `postgresql.auto.conf`, with its `include`, `include_if_exists` and
  `include_dir` lines. A file with a syntax error applies nothing, and
  `pg_file_settings` stops at the error, as PostgreSQL does.
- Without `--hba-file`, a `pg_hba.conf` and `pg_ident.conf` in the data
  directory decide logins, as PostgreSQL reads its default files (Joe,
  2026-10-23). A store without them is served as before.

#### Fixed

- Fixes RP-162 (`tests/backlog/test_rp_batch33.py::test_scenario[every parameter answers SHOW]`,
  `tests/backlog/test_rp_batch35.py::test_scenario[the server's own files and port]`,
  `[the main configuration file and what it includes]` and
  `::test_a_store_s_pg_hba_conf_decides_logins`): `SHOW` of ten parameters
  was 42704. `is_superuser`, `default_with_oids` and `ssl_renegotiation_limit`
  now answer, and so do:
  - `data_directory` (the store's directory);
  - `config_file`, `hba_file` and `ident_file`, under it unless
    `--hba-file` names others;
  - `port`, the port the server listens on;
  - `unix_socket_directories`, which is empty, since this server makes no
    socket;
  - `log_timezone`.

  `pg_settings` gives each its PostgreSQL row and source (`override`,
  `command line`).
- Fixes RP-170 (`tests/backlog/test_rp_batch35.py::test_scenario[the parameters only a superuser may read]`):
  any role could read the 21 parameters PostgreSQL keeps for a superuser or
  a member of `pg_read_all_settings`: the file locations, the library
  lists, the SSL settings and the rest. `SHOW` and `current_setting()` of
  one are now 42501, and `pg_settings` leaves them out.
- Fixes RP-171 (`tests/backlog/test_rp_batch35.py::test_scenario[a superuser parameter needs the SET privilege]`):
  any role could set a parameter of `superuser` context, such as
  `log_min_duration_statement` or `lc_messages`. A role without the `SET`
  privilege on one now gets 42501 from SET, SET LOCAL, `SET ... TO
  DEFAULT`, RESET and `set_config`, checked for the role in effect. A
  database's or role's own defaults still apply at login.
- Fixes RP-168 (`tests/backlog/test_rp_batch34.py::test_scenario[a default's literal for an "any" argument is bare]`):
  a default's literal passed to a variadic `"any"` parameter (`concat`,
  `concat_ws`, `format`) printed as `'a'::text`. It is now bare, as
  PostgreSQL prints it.
