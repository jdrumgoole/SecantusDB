# statement_timeout and lock_timeout, measured against PostgreSQL 14.
SHOW statement_timeout
SET statement_timeout = 100
SELECT pg_sleep(1)
SHOW statement_timeout
SET statement_timeout = '2s'
SHOW statement_timeout
SET statement_timeout = 'x'
SET lock_timeout = -1
SET lock_timeout = '1min'
SHOW lock_timeout
SELECT current_setting('statement_timeout')
RESET statement_timeout
SHOW statement_timeout
SELECT pg_sleep(0.2)
RESET lock_timeout
SHOW lock_timeout
