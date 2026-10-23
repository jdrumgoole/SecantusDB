### Rust PostgreSQL server: text-search configurations of your own

`CREATE`, `ALTER` and `DROP TEXT SEARCH CONFIGURATION` work as in PostgreSQL
15.19. Every text-search function now reads a configuration from its map:
the dictionaries each token type is offered to, in order. That holds for
the built-in configurations as dumped and for the ones you make.

#### Added

- `CREATE TEXT SEARCH CONFIGURATION name (PARSER = default | COPY = c)`,
  `ALTER ... ADD / ALTER / DROP MAPPING` and `ALTER MAPPING REPLACE`,
  `RENAME TO`, `OWNER TO`, `SET SCHEMA`, `COMMENT ON` and `DROP [IF EXISTS]
  ... [CASCADE]`. Fixes RP-072
  (`tests/backlog/test_rp_found_2026_10_20.py::test_statements_not_refused`,
  `tests/backlog/test_rp_batch34.py::test_scenario[CREATE TEXT SEARCH CONFIGURATION and its map]`,
  `[a configuration's name, owner, schema and comment]`,
  `[DROP TEXT SEARCH CONFIGURATION and what depends on it]` and
  `[a configuration's dependencies]`).
  - **Where it reaches.** A configuration is a schema's object. It appears
    in `pg_ts_config` and `pg_ts_config_map` and resolves through
    `regconfig` and the search path, as do `default_text_search_config`,
    `get_current_ts_config()`, `to_tsvector`, the `*_tsquery` functions,
    `ts_headline` and `ts_debug`.
  - **The map.** A token type with no mapping takes no position, as
    PostgreSQL's parser does.
  - **Errors** are PostgreSQL's. A duplicate name or mapping is its unique
    violation on `pg_ts_config_cfgname_index` or `pg_ts_config_map_index`.
  - **Dependents.** DROP refuses (2BP01), or with CASCADE drops, what names
    the configuration by a constant: a column default, a generated column,
    a check constraint, an index expression, a view. DROP SCHEMA CASCADE
    takes a schema's configurations with it.
  - **`pg_depend`** records each configuration's schema and dictionaries.

#### Fixed

- Fixes RP-166 (`tests/backlog/test_rp_batch34.py::test_scenario[DROP COLLATION CASCADE drops each column that uses it]`):
  `DROP COLLATION ... CASCADE` left all but the last of a table's columns
  that used the collation, and sent no notice. Each column-dropping
  statement read the table as the one before had found it. The columns now
  go in one statement per table, in PostgreSQL's order, with its notice.
- Fixes RP-167 (`tests/backlog/test_rp_batch34.py::test_scenario[a column default's call prints its literals' types]`):
  a column default calling a function printed each string literal as
  `text`. `to_tsvector('english', 'x')` read back as
  `to_tsvector('english'::text, 'x'::text)`, a call with no such overload.
  Literals now take the parameter types of the overload PostgreSQL picks
  (`'english'::regconfig`).

#### Known

- RP-165, filed: a built-in configuration cannot be renamed, remapped or
  dropped here, where a PostgreSQL superuser may
  (`tests/backlog/test_rp_batch34.py::test_scenario[a built-in configuration can be altered]`).
- RP-168, filed: a default's literal passed to a variadic `"any"`
  parameter still prints as `'a'::text`
  (`tests/backlog/test_rp_batch34.py::test_scenario[a default's literal for an "any" argument is bare]`).
- RP-169, filed: eleven statements PostgreSQL runs are still refused, among
  them `ALTER EXTENSION ... UPDATE`, `CREATE OPERATOR CLASS` and `CREATE
  SUBSCRIPTION`
  (`tests/backlog/test_rp_batch34.py::test_scenario[statements PostgreSQL runs that are refused]`).
