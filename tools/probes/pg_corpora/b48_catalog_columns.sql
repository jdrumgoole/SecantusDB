# reference-version: 15
# Batch 48: `SELECT *` over the system catalogs has PostgreSQL 15's column
# set and order (pg_class lacked relfrozenxid / relminmxid and ordered its
# columns differently, pg_attribute lacked attalign / attbyval / ...,
# pg_database carried PostgreSQL 16's daticurules, ...). The column LISTS
# are pinned by the slice test `test_select_star_over_catalogs_has_pg15_columns`
# (this probe compares values, not names); here, the added columns' values.
SELECT relfrozenxid::text <> '0', relminmxid::text <> '0', relpersistence, relkind FROM pg_class WHERE relname = 'b48t'
SELECT relfrozenxid::text, relminmxid::text FROM pg_class WHERE relname = 'b48t_pkey'
SELECT attname, attcacheoff, attbyval, attalign, attmissingval IS NULL FROM pg_attribute WHERE attrelid = 'b48t'::regclass AND attnum > 0 ORDER BY attnum
SELECT typowner = (SELECT relowner FROM pg_class WHERE relname = 'b48t'), typdefaultbin IS NULL FROM pg_type WHERE typname = 'b48e'
SELECT pronargdefaults, proargdefaults IS NULL, protrftypes IS NULL, probin IS NULL FROM pg_proc WHERE proname = 'b48tf'
SELECT tgattr::text, tgargs, tgqual IS NULL, tgoldtable IS NULL, tgnewtable IS NULL FROM pg_trigger WHERE tgname = 'b48trg'
SELECT lanname, lanispl, lanowner, lanvalidator <> 0 FROM pg_language ORDER BY 1
SELECT nspacl IS NULL FROM pg_namespace WHERE nspname = 'b48none'
