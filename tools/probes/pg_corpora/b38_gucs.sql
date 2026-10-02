# reference-version: 15
# SET LOCAL, a SET inside a block, set_config(..., true): undone or kept at
# the block's end. pg_settings, and ALTER DATABASE's checks.
SET application_name = 'b38a'
BEGIN
SET LOCAL application_name = 'b38b'
SHOW application_name
COMMIT
SHOW application_name
BEGIN
SET application_name = 'b38c'
SHOW application_name
ROLLBACK
SHOW application_name
BEGIN
SET application_name = 'b38d'
COMMIT
SHOW application_name
BEGIN
SET application_name = 'b38e'
SET LOCAL application_name = 'b38f'
SHOW application_name
COMMIT
SHOW application_name
BEGIN
SELECT set_config('application_name', 'b38g', true)
SHOW application_name
ROLLBACK
SHOW application_name
SET LOCAL application_name = 'b38h'
SHOW application_name
BEGIN
SET LOCAL statement_timeout = '5s'
SHOW statement_timeout
ROLLBACK
SHOW statement_timeout
SELECT setting FROM pg_catalog.pg_settings WHERE name = 'default_transaction_isolation'
SET default_transaction_isolation = 'serializable'
SELECT setting, source FROM pg_catalog.pg_settings WHERE name = 'default_transaction_isolation'
RESET default_transaction_isolation
SELECT setting FROM pg_settings WHERE name = 'default_transaction_isolation'
SELECT name, setting FROM pg_settings WHERE lower(name) = 'datestyle'
SELECT count(*) > 0 FROM pg_settings WHERE name = 'application_name'
ALTER DATABASE b38_no_such_db SET work_mem = '1MB'
ALTER DATABASE b38_no_such_db RESET ALL
