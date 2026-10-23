### Rust PostgreSQL server: dictionaries, operator classes, subscriptions and transforms

Four kinds of statement that `pg_dump` output carries, and that were refused
by name, now run as PostgreSQL 15.19 runs them. `CREATE / ALTER / DROP TEXT
SEARCH DICTIONARY`, `OPERATOR FAMILY / CLASS`, `SUBSCRIPTION` and `CREATE /
DROP TRANSFORM` each check what PostgreSQL checks, in its order, and keep
their catalog rows as PostgreSQL keeps them.

#### Added

- Most of RP-169
  (`tests/backlog/test_rp_batch34.py::test_scenario[statements PostgreSQL runs that are refused]`,
  and `tests/backlog/test_rp_batch51.py::test_scenario[...]`):
  - **Text search dictionaries** over the `simple`, `snowball` and
    `synonym` templates (`a simple, a snowball and a synonym dictionary`,
    `a configuration lexes through user dictionaries in order`, `a
    dictionary's errors`, `rename, owner, schema and comment`, `a built-in
    dictionary changed`). Their options are checked as each template's
    `init` checks them. `ts_lexize`, `to_tsvector` and `ts_debug` lex
    through them. `synonym_sample.syn` is vendored with the stop-word files.
  - **Operator families and classes** of the btree and hash methods (`an
    operator family and class in the catalogs`, `an implicit family, a hash
    class and a storage type`, `an operator class's errors`, `alter
    operator family`, `rename, owner, schema, comment and drop of a class`,
    `a class's functions and operators are its dependencies`). They are
    checked as `opclasscmds.c` checks them, and their members are hard or
    soft as `btadjustmembers` makes them. Their rows appear in
    `pg_opfamily`, `pg_opclass`, `pg_amop` and `pg_amproc`. CREATE INDEX
    takes a user class, and refuses one whose family lacks support function
    1 (`XX000`). `pg_index.indclass` names a key's explicit class, built-in
    ones such as `text_pattern_ops` included; it used to name the type's
    default class.
  - **Subscriptions** created with `connect = false` (`a subscription
    created without connecting`, `a subscription's errors`). Options are
    parsed by a port of `parse_subscription_options`, and the conninfo is
    checked as libpq parses one. Each subscription has its
    `pg_<oid>` replication origin and its rows in `pg_stat_subscription` and
    `pg_stat_subscription_stats`. Nothing connects to a publisher (a scope
    decision in the backlog): what would connect is refused by name.
  - **Transforms** (`a transform`, `a transform's errors`). Their functions
    are checked as `CreateTransform` checks them. DROP TYPE refuses while
    one names the type, and CASCADE drops it.

#### Fixed

- Fixes RP-201
  (`tests/backlog/test_rp_batch51.py::test_scenario[shobj_description reads a shared object's comment]`):
  `shobj_description` answered NULL for a database's or a role's comment.
  It now reads `pg_shdescription`.

#### Known

- RP-169, what is left: operator classes of GiST, GIN, SP-GiST and BRIN,
  the `ispell` and `thesaurus` dictionary templates, and `pg_subscription`
  across databases.
- RP-199: `pg_describe_object` and the other object-address functions are
  refused.
- RP-200: inside a block, `pg_get_indexdef` answers NULL for an index the
  block created.
