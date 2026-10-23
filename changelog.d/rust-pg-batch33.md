### Rust PostgreSQL server: ALTER SYSTEM, and settings read as PostgreSQL reads them

`ALTER SYSTEM` writes the server's `postgresql.auto.conf`, and
`pg_reload_conf()` puts it in force, as in PostgreSQL 15.19. A setting's
value is now checked against its type, unit and range and shown as
PostgreSQL shows it. Each setting also remembers where its value came from.

#### Added

- `ALTER SYSTEM SET name = value`, `TO DEFAULT`, `RESET name` and
  `RESET ALL` (`tests/backlog/test_rp_batch33.py::test_scenario[ALTER SYSTEM takes effect at a reload]`,
  `[a reload leaves what outranks the file]`,
  `[the file ALTER SYSTEM writes]`, `[what ALTER SYSTEM refuses]` and
  `[a parameter only a restart changes is pending]`). RP-072 is down to
  `CREATE TEXT SEARCH CONFIGURATION`.
  - **The file.** `postgresql.auto.conf` lives in the store's directory,
    written at once and atomically, line for line as PostgreSQL writes it.
    The server reads it at start, so it survives a restart.
  - **When a change takes effect.** Nothing changes until
    `pg_reload_conf()`. Each session then takes the new values at its next
    message, unless something higher outranks the file: its own SET, a
    database or role default, or a value from the startup packet. The
    reload also reaches the values an open transaction would restore, so
    a COMMIT after a `SET LOCAL` lands on the new value.
  - **Restart-only parameters** keep their values and read
    `pending_restart`, which new sessions inherit, as in PostgreSQL. A
    `sighup` parameter changes in every session.
  - **The catalogs.** `pg_file_settings` lists the file's lines as they
    are now, with `applied` and `error`. `pg_settings` reports
    `configuration file` with `sourcefile` and `sourceline`, shown only
    to a superuser or a member of `pg_read_all_settings`.
  - **Refusals, with PostgreSQL's codes:**
    - 25001 in a transaction block;
    - 42704 for an unknown parameter, RESET included;
    - 22023 for a value the parameter refuses;
    - 55P02 for a parameter the file may not hold;
    - 42501 for a role that is not a superuser.

#### Fixed

- Fixes RP-159 (`tests/backlog/test_rp_batch33.py::test_scenario[a setting's value is read and shown as its type does]`):
  `SET` and `set_config()` accepted any text for a typed parameter and
  stored it as given. `set work_mem = 'abc'` succeeded, and
  `set work_mem = 8192` then read back as `8192`. Values are now parsed as
  PostgreSQL's parser parses them, from the `pg_settings` dump:
  - integers in octal, hex or fractions, rounded half to even;
  - units, with PostgreSQL's hint for one the parameter does not take;
  - the range check, with its exact message;
  - booleans by prefix;
  - enums in any case, with their hidden spellings.

  `SHOW` answers in the largest unit that divides the value (`8MB`), and a
  real in `%g` form.
- Fixes RP-160 (`tests/backlog/test_rp_batch33.py::test_scenario[a SET's value list is one text]`):
  - A SET of more than one value was accepted for any parameter. It is
    now `SET x takes only one argument` (22023) unless the parameter takes
    a list.
  - A list of identifiers lost its quoting, so `set search_path = "Rp33
    S"` could not reach the schema. Each value is now quoted as
    `quote_identifier` quotes it, so `set search_path = 'a, b'` names one
    schema, `"a, b"`.
- Fixes RP-161 (`tests/backlog/test_rp_batch33.py::test_scenario[where a setting's value came from]`):
  `pg_settings.source` was guessed by comparing values, so a session that
  SET a parameter to its default read `default`. Each value now carries
  its source, and transactions and savepoints restore it with the value:
  - a startup-packet setting reads `client`, and RESET and RESET ALL
    return to it;
  - `SET ... TO DEFAULT` takes its reset value's source;
  - the `transaction_*` parameters read `override`, as PostgreSQL's do.

  A RESET inside a block is now undone by ROLLBACK.
- Fixes RP-163 (`tests/backlog/test_rp_batch33.py::test_scenario[the system relations PUBLIC may not read]`):
  any role could read `pg_authid`, `pg_shadow`, `pg_statistic`,
  `pg_hba_file_rules` and the rest of the 14 system relations PostgreSQL
  revokes from PUBLIC. Each is now 42501 for a role without the grant,
  whether named bare or with `pg_catalog.`. Every `pg_subscription` column
  except `subconninfo` stays PUBLIC's, as in PostgreSQL.
- `pg_reload_conf()` reloads its own server from any worker thread. Before,
  its hook was installed only on the thread that ran the session's
  startup.

#### Known

- RP-162, filed: ten parameters, among them `is_superuser`, `port` and
  `data_directory`, answer 42704 to SHOW
  (`tests/backlog/test_rp_batch33.py::test_scenario[every parameter answers SHOW]`).
- RP-164, filed: `GRANT SET / ALTER SYSTEM ON PARAMETER` is refused, so
  ALTER SYSTEM is a superuser's alone
  (`tests/backlog/test_rp_batch33.py::test_scenario[GRANT SET and ALTER SYSTEM ON PARAMETER]`).
